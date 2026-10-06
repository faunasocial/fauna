"""Linked nests — the user-facing per-user-multi-homing surface
(docs/goal/behavior/linked-nests.md).

A page in the user's *own* settings (not the admin shell) where a user links one
of their nests to sync their account's content, lists their linked nests, and
unlinks them. Renders the shared `LinkedNestsMachine` (libs/fauna-client-pair)
over UniFFI/wasm: Link → `fauna.pair.add`, Refresh → `fauna.pair.list`, Unlink →
`fauna.pair.revoke`. The admin's only pairing control is the operator
`admin-service-pairing-toggle` (admin.md § N Nest, post-2026-06-04 redesign), exercised here for the
operator-knob-off rejection path.

tier_3 (full stack — real `fauna-nest` binary; the pairing kinds + the operator
`pairing` service knob are nest-side, so a mocked backend can't catch flow
breaks between the shared machine and the handlers). linux leads; the other five
apps lift this shape (priority #1).
"""
import time

import pytest

from common.auth import register_user

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# A syntactically-valid nest identity: a 32-byte Ed25519 public key as 64 hex
# chars. The nest's `fauna.pair.add` stores it without resolving a live nest, so
# any valid-shape key drives the link/list/unlink round-trip.
NEST_ID_A = "ab" * 32
NEST_ID_B = "cd" * 32


@pytest.mark.feature("nests-and-trust")
def test_linked_nests_page_renders(logged_in_app):
    """The Linked-nests page is reachable from user settings and shows the
    add-a-nest affordance with an initially-empty list."""
    app = logged_in_app
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page not reachable. error: {app.error_text()!r}"
    )
    # A fresh user has no pairings.
    assert app.linked_nests.wait_for_pairing_count(0, timeout=6.0), (
        f"expected an empty pairing list, got {app.linked_nests.pairing_count()}. "
        f"error: {app.linked_nests.page_error_text(timeout=2.0)!r}"
    )


@pytest.mark.feature("nests-and-trust")
def test_link_list_unlink_round_trip(logged_in_app):
    """Link a nest → it appears in the list (its id rendered) → unlink it → the
    list returns to empty. Exercises Link/Refresh/Unlink end-to-end against the
    real nest (`fauna.pair.{add,list,revoke}`)."""
    app = logged_in_app
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    # Start from a known-empty list (a prior test may share the session nest).
    for i in range(app.linked_nests.pairing_count()):
        app.linked_nests.unlink(0)
    assert app.linked_nests.wait_for_pairing_count(0, timeout=8.0)

    app.linked_nests.link(NEST_ID_A)
    assert app.linked_nests.wait_for_pairing_count(1, timeout=12.0), (
        f"linking should add one pairing. error: {app.linked_nests.page_error_text()!r}"
    )
    # The row renders the linked nest's id (abbreviated hex). The abbreviation
    # is the client's call, but it must derive from the linked key — assert the
    # rendered id shares the key's leading hex.
    rendered = app.linked_nests.nest_ids()[0].lower().replace(" ", "")
    assert "abab" in rendered or rendered.startswith("ab"), (
        f"row nest-id {rendered!r} should derive from the linked key {NEST_ID_A!r}"
    )

    app.linked_nests.unlink(0)
    assert app.linked_nests.wait_for_pairing_count(0, timeout=12.0), (
        "unlinking should empty the list. "
        f"error: {app.linked_nests.page_error_text()!r}"
    )


@pytest.fixture(scope="function")
def pairing_nest_b(request, nest_mode, tmp_path_factory):
    """The SECOND nest the link form points at — the user's other box.

    This was inlined in the test body: `find_free_port()`, `start_nest`, and a
    hand-rolled terminate/wait/kill teardown, which is `_make_nest` copied by
    hand and therefore invisible to the mode axis (`testing.md` § Default app and
    nest mode, ruling (1) — every nest the harness starts is the mode provider's
    to start). It asks for nothing, so it is the zero-option call in every mode.

    The client, not a nest, dials B here: the app enters B's `url` in the link
    form and opens its own second authenticated WS-RPC connection to it. That is
    the HOST-DRIVEN two-nest shape ruling (2) keeps in docker, not the
    nest-dials-nest shape class (8) excludes — nothing hands one nest the other's
    `peer_url`.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "pairing-nest-b",
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.feature("nests-and-trust")
def test_link_both_seeds_both_nests(
    logged_in_app, nest_instance, test_user, pairing_nest_b
):
    """One action links two of the user's nests, seeding the authorization row on
    BOTH (linked-nests.md § "One action seeds both ends").

    The user — logged into nest A — enters a SECOND nest B's *address* in the
    link form. The shared `LinkedNestsMachine`'s `LinkBoth` classifies the input
    as a URL (not a 64-hex identity), opens an authenticated connection to B with
    the user's same keypair, discovers both nests' ids via `fauna.nest.info`, and
    writes the reciprocal `fauna.pair.add` rows: B's id on A, A's id on B.

    The machine writes B's row first, then A's, then re-lists — so A showing the
    new pairing **with no error** transitively confirms B's row landed
    (`link_both` only completes when both adds succeed). The direct cross-process
    proof that *both* nests' `nest_pairings` gain the row is the Rust tier_3
    `bins/fauna-nest/tests/conformance_cross_nest_pairing_client.rs`; this test
    covers the linux GTK form wiring + the real second connection end-to-end.

    Linux leads; web now joins it — the SPA opens the second authenticated
    WS-RPC client to nest B by minting B's bearer over the CORS-exempt anonymous
    WS (`fauna.auth.{challenge,verify}`, `challengeVerify`), the path the cross-origin HTTP
    `/api/v1/auth/token` fetch could not take (transport.md § Pre-identity;
    tracked internally).
    """
    app = logged_in_app

    # A second nest B the user is also reachable on: register the app's OWN
    # identity there, so the client's second connection authenticates and
    # `fauna.pair.add` passes the User-class gate.
    nest_b = pairing_nest_b
    # Register over WS-RPC (`fauna.admin.users.create`) with the admin's
    # signing key — the legacy `admin_token` HTTP path (`POST /admin/api/users`)
    # was ripped, so a token-only call now hits the SPA fallback
    # (200) and silently fails to register. Mirrors the conftest fixtures.
    register_user(
        nest_b["port"],
        test_user["actor_id_hex"],
        admin_signing_key=nest_b["admin"]["signing_key"],
    )

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    # Start from a known-empty list (the session nest is shared).
    for _ in range(app.linked_nests.pairing_count()):
        app.linked_nests.unlink(0)
    assert app.linked_nests.wait_for_pairing_count(0, timeout=8.0)

    # One action: link B by its address → both ends seeded.
    app.linked_nests.link(nest_b["url"])
    assert app.linked_nests.wait_for_pairing_count(1, timeout=15.0), (
        "one both-ends link should add one pairing on the connected nest. "
        f"error: {app.linked_nests.page_error_text()!r}"
    )
    # The connected nest's row names the OTHER nest (B) — its rendered id
    # derives from B's nest identity (abbreviated leading hex).
    rendered = app.linked_nests.nest_ids()[0].lower().replace(" ", "")
    b_id = nest_b["nest_id"].lower()
    assert b_id, "nest B did not report a nest_id"
    assert rendered.startswith(b_id[:8]) or b_id[:8] in rendered, (
        f"row nest-id {rendered!r} should derive from nest B's id {b_id!r}"
    )
    # No error surfaced — both adds (B then A) succeeded.
    assert not app.linked_nests.page_error_text(timeout=2.0), (
        "a successful both-ends link must surface no error"
    )

    # Leave the shared session nest as found.
    app.linked_nests.unlink(0)
    app.linked_nests.wait_for_pairing_count(0, timeout=8.0)


@pytest.mark.feature("nests-and-trust")
def test_admin_knob_off_rejects_link(admin_app):
    """When the operator disables the `pairing` service, `fauna.pair.add` is
    rejected and the surface shows the policy error instead of adding a row.

    The admin toggles the pairing service off (admin-nest after the 2026-06-04
    per-page-services redesign; admin-services on clients that haven't lifted it
    — navigate_to_pairing_control handles both), then attempts a link from their
    own Linked-nests page; the rejection (`fauna.pair.pairing_disabled`) surfaces
    in `error-message` and no row is added. Restores the knob (default-on) at the
    end so the shared session nest is left as found.
    """
    app = admin_app
    # Disable pairing nest-wide via the operator toggle.
    app.admin.navigate_to_pairing_control()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not app.admin.service_toggle_present("pairing"):
        time.sleep(0.3)
    assert app.admin.service_toggle_present("pairing"), (
        f"admin-service-pairing-toggle missing. error: {app.error_text()!r}"
    )
    before = app.admin.service_status_text("pairing").strip()
    app.admin.toggle_service("pairing")
    # Wait for the status badge to flip (the update + services refetch round-trip).
    deadline = time.monotonic() + 15.0
    while (
        time.monotonic() < deadline
        and app.admin.service_status_text("pairing").strip() == before
    ):
        time.sleep(0.3)

    try:
        app.linked_nests.navigate()
        assert app.linked_nests.is_page_visible()
        for i in range(app.linked_nests.pairing_count()):
            app.linked_nests.unlink(0)
        app.linked_nests.link(NEST_ID_B)
        err = app.linked_nests.page_error_text(timeout=10.0)
        assert err, "a link attempt with pairing disabled should surface an error"
        assert app.linked_nests.pairing_count() == 0, (
            "no pairing should be added when the operator knob is off"
        )
    finally:
        # Restore the default-on knob for the shared session nest.
        app.admin.navigate_to_pairing_control()
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline and not app.admin.service_toggle_present("pairing"):
            time.sleep(0.3)
        if app.admin.service_status_text("pairing").strip() != before:
            app.admin.toggle_service("pairing")

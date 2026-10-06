"""tier_3 e2e: the admin can rotate an **approved** bridge's service-user key
from the admin bridges page's Approved-bridges roster.

Proves the UI→RPC leg the shared machinery below the button already covers at
tier_4 (``tests/e2e-unified/tests/platform/docker/test_bridge_rekey_rotation.py``
+ the in-process ``test_mail_bridge_rekey.py``): the ``admin-bridges-approved-card``
roster surfaces an approved bridge (``list_service_users(status="approved")``),
its ``admin-bridges-approved-rotate-button`` opens the ``admin-bridges-rotate-confirm``
dialog (which shows the rotation warning and **no DKIM warning for any role** —
the nest holds every DKIM key, so a re-key touches none), and confirming
dispatches ``fauna.bridges.revoke_service_user`` through the shared
``BridgeApprovalMachine`` ``Rotate`` action so the bridge is revoked and drops
from the roster, leaving the nest's DKIM selectors exactly as they were.

Setup enrolls and approves the bridge over the bridge's and the admin's own
WS-RPC kinds (fixture arrangement — the approve card is covered by
``test_admin_bridges_pending.py``); the action under test is the rotate. Setup
cannot go through the pending card here: the session nest has mail enabled by
the time this runs, and the nest auto-approves a loopback mail bridge then
(``mail-bridge-lifecycle.md`` § Onboarding auto-approval). ``nest_instance`` is
session-scoped, so we seed a unique random pubkey and assert *that* card rather
than an absolute count.

Scoped to **all 7 apps** — every one has now lifted the roster + rotate UI
(android's ``--client android`` run is host-emulator-gated, same as
``test_admin_bridges_pending.py`` — unverified on the primary dev machine,
compile-verified only) (``admin.md``
§ Approved-bridges roster; tracked internally, Phase 3).
"""

import secrets
import time

import pytest

from i18n.strings import S
from helpers.bridge_enrollment import enroll_and_approve_bridge

# `tui` added 2026-07-29 with the page itself (`apps/fauna-tui/src/admin/bridges.rs`),
# rotate confirm included as an inline reveal per `admin.md` § Approved-bridges roster.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


def _seed_approved_bridge(nest_instance, role: str = "mta") -> str:
    """Enroll and approve a bridge (setup) over ``request_enrollment`` +
    ``approve_pending_bridge``. Nothing proves possession of the key at
    enrollment, so a synthetic 32-byte value is enough to exercise the roster +
    rotate flow. Returns the hex pubkey."""
    pubkey = secrets.token_bytes(32)
    enroll_and_approve_bridge(
        nest_instance["url"], nest_instance["admin"]["signing_key"], pubkey, role,
        f"{role}-{secrets.token_hex(4)}",
    )
    return pubkey.hex()


def _dkim_selectors(nest_instance, domain: str) -> list:
    """The nest-held DKIM selectors for ``domain`` (admin side-channel read via
    ``fauna.bridges.list_dkim_selectors`` — an observation, not a mutation),
    as sorted ``(selector, public_dns_value)`` pairs."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = nest_instance["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    ) as admin_ws:
        reply = admin_ws.call("fauna.bridges.list_dkim_selectors", {"domain": domain})
    return sorted((r["selector"], r["public_dns_value"]) for r in reply["selectors"])


def _add_mail_domain(nest_instance) -> str:
    """Add a unique mail domain (setup) — adding it is what makes the nest mint
    its DKIM key. Returns the domain."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    domain = f"rotate-{secrets.token_hex(4)}.test"
    sk = nest_instance["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    ) as admin_ws:
        admin_ws.call(
            "fauna.bridges.add_local_domain",
            {"domain": domain, "mta_sts_cert_mode": "per_host"},
        )
    return domain


def _find_card(admin_app, id_prefix: str, pubkey_hex: str) -> int | None:
    """Index of the ``{id_prefix}-pubkey-hex`` card containing ``pubkey_hex``."""
    count = admin_app.driver.count(f"{id_prefix}-pubkey-hex")
    for i in range(count):
        txt = admin_app.driver.get_text(f"{id_prefix}-pubkey-hex", index=i) or ""
        if pubkey_hex in txt:
            return i
    return None


def _poll_card(admin_app, id_prefix: str, pubkey_hex: str, *, want: bool) -> int | None:
    """Poll up to 10 s for the card to be present (``want=True``) or gone."""
    deadline = time.time() + 10.0
    while time.time() < deadline:
        idx = _find_card(admin_app, id_prefix, pubkey_hex)
        if (idx is not None) == want:
            return idx
        time.sleep(0.5)
    return _find_card(admin_app, id_prefix, pubkey_hex)


@pytest.mark.feature("admin-bridges")
def test_admin_rotates_approved_mta_service_user_key(admin_app, nest_instance):
    """An approved MTA bridge appears in the Approved roster; rotating it (the
    dialog shows no DKIM warning) revokes it so it drops from the roster, and the
    nest's DKIM selectors are unchanged — a re-key touches no DKIM key."""
    domain = _add_mail_domain(nest_instance)
    selectors_before = _dkim_selectors(nest_instance, domain)
    assert selectors_before, f"adding {domain!r} must mint the nest-held DKIM key"
    pubkey_hex = _seed_approved_bridge(nest_instance, "mta")
    admin_app.admin.navigate_bridges_pending()

    # --- the approved roster now surfaces it (list_service_users(status=approved)).
    aidx = _poll_card(admin_app, "admin-bridges-approved", pubkey_hex, want=True)
    assert aidx is not None, (
        f"approved bridge {pubkey_hex[:16]}… not in the Approved roster "
        f"(error: {admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
    )
    role_txt = admin_app.driver.get_text("admin-bridges-approved-role", index=aidx) or ""
    assert "mta" in role_txt.lower(), f"approved role {role_txt!r} should mention mta"

    # --- action under test: rotate → confirm dialog → confirm.
    admin_app.driver.click("admin-bridges-approved-rotate-button", index=aidx)
    assert admin_app.driver.is_visible("admin-bridges-rotate-warning-text"), (
        "rotate confirm dialog should show the rotation warning"
    )
    assert admin_app.driver.is_absent("admin-bridges-rotate-dkim-warning-text"), (
        "an MTA re-key touches no DKIM key (the nest holds them all), so the "
        "dialog must not warn about DKIM"
    )
    admin_app.driver.click("admin-bridges-rotate-confirm-button")

    # --- the rotated bridge is revoked → drops from the approved roster.
    still = _poll_card(admin_app, "admin-bridges-approved", pubkey_hex, want=False)
    assert still is None, (
        f"rotated bridge {pubkey_hex[:16]}… still in the Approved roster "
        f"(error: {admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
    )
    assert _dkim_selectors(nest_instance, domain) == selectors_before, (
        "rotating the MTA's service-user key must leave the nest's DKIM selectors "
        "and published records untouched"
    )


@pytest.mark.feature("admin-bridges")
def test_mda_rotation_hides_dkim_warning(admin_app, nest_instance):
    """An MDA bridge's rotate dialog shows the base warning and no DKIM warning —
    the same as every other role (admin.md § Approved-bridges roster)."""
    pubkey_hex = _seed_approved_bridge(nest_instance, "mda")
    admin_app.admin.navigate_bridges_pending()

    aidx = _poll_card(admin_app, "admin-bridges-approved", pubkey_hex, want=True)
    assert aidx is not None, f"approved mda bridge {pubkey_hex[:16]}… not in roster"

    admin_app.driver.click("admin-bridges-approved-rotate-button", index=aidx)
    assert admin_app.driver.is_visible("admin-bridges-rotate-warning-text"), (
        "the base rotation warning shows for every role"
    )
    assert admin_app.driver.is_absent("admin-bridges-rotate-dkim-warning-text"), (
        "no role's rotation shows a DKIM warning"
    )
    # Cancel — this test asserts the dialog contents, not the rotation itself.
    admin_app.driver.click("admin-bridges-rotate-cancel-button")

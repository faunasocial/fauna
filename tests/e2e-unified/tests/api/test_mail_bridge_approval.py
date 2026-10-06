"""tier_3 e2e for the Stage-1 bridge-approval + set_mail_enabled WS-RPC surface
(``fauna.bridges.{list_pending_bridges,approve_pending_bridge,
reject_pending_bridge,set_mail_enabled}``;
``docs/goal/behavior/mail-bridge-lifecycle.md`` § Pending approval / § Default-off).

These four kinds are **Admin-class**. They take an enrolled bridge from
``pending`` → ``approved`` / ``→ revoked`` (wrapping the DB enrollment methods),
and give the admin the deployment-wide mail-enable toggle. This file proves the
real router dispatches the registered kind strings, the allowlist gates them to
Admin, and the validation wire path works end-to-end over the real socket —
coverage the in-process handler unit tests (which call the handlers directly,
bypassing the router + allowlist string match) cannot give.

A pending bridge is seeded over the bridge's own anonymous enrollment kind
(``fauna.bridges.request_enrollment``) — the one enrollment surface
(``mail-bridge-lifecycle.md`` § Cold boot). That kind auto-approves an MTA/MDA
once the deployment's mail (or a DAV) axis is on (§ Onboarding auto-approval),
and the session ``nest_instance`` has mail on as soon as any mail-bridge fixture
ran before this module — so the tests that need a row to land ``pending`` seed
it on ``dedicated_no_mail_nest``, where no axis was ever enabled. Every test
still uses a random pubkey + ``bridge_id`` and filters list results to its own
row.
"""

import secrets
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import create_actor_and_register
from conftest import live_admin_token
from helpers.bridge_enrollment import enroll_bridge

pytestmark = pytest.mark.tier_3


def _admin_client(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _seed_pending(nest_instance, role: str) -> tuple[bytes, str]:
    """Enroll a pending bridge over the anonymous ``request_enrollment`` kind.
    Returns ``(ed25519_pubkey_bytes, bridge_id)``. The pubkey is random —
    enrollment only stores it (the bridge would later prove possession at its
    challenge-response auth), so a synthetic 32-byte value is enough to exercise
    approval."""
    pubkey = secrets.token_bytes(32)
    bridge_id = f"{role}-{secrets.token_hex(4)}"
    status = enroll_bridge(nest_instance["url"], pubkey, role, bridge_id)
    assert status == "pending", f"seed must land pending, got {status!r} (mail enabled?)"
    return pubkey, bridge_id


def test_a_freshly_minted_admin_bearer_authorizes_the_admin_http_route(nest_instance):
    """`conftest.live_admin_token` really re-mints, and what it mints really
    carries admin authority.

    Both halves matter and neither is obvious. The nest mints bearers with a
    one-hour TTL (`auth_core.rs::TOKEN_TTL_SECS`) while `nest_instance` is
    session-scoped, so any fixture first reached past that hour used a dead
    token and `AdminBearerAuth` answered 401 — an `ERROR at setup` across the
    whole bridge-dependent family, deterministic in a long sweep and invisible
    in a solo run. The fix only works because admin authority lives on the
    *actor* (`auth.rs`: validate the token, then `db.is_admin(actor_id)`), not
    inside the token — so a bearer re-minted from the admin's signing key over
    `fauna.auth.handshake` is as privileged as the one `claim_admin` returned.
    If that ever stops being true, this pin goes red instead of a mail sweep.
    """
    fresh = live_admin_token(nest_instance)
    assert fresh, "a claimed nest must yield a live admin bearer"
    assert fresh != nest_instance["admin"]["token"], (
        "live_admin_token must MINT, not hand back the cached claim-time token — "
        "returning the cache would silently restore the one-hour expiry bug"
    )

    # And it authorizes nest's remaining admin-bearer HTTP route (the bulk
    # export; `api-layers.md` § the HTTP residue tables).
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/admin/export/all",
        headers={"Authorization": f"Bearer {fresh}"},
        method="GET",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, (
            f"a freshly minted admin bearer must authorize the admin HTTP route, "
            f"got {resp.status}"
        )


def _find(rows, bridge_id):
    return next((r for r in rows if r["bridge_id"] == bridge_id), None)


@pytest.mark.feature("admin-bridges")
def test_list_then_approve_flow(dedicated_no_mail_nest):
    """seed pending → list_pending_bridges sees it → approve → it leaves the
    pending feed and appears approved in list_service_users."""
    admin = _admin_client(dedicated_no_mail_nest)
    pubkey, bridge_id = _seed_pending(dedicated_no_mail_nest, "mta")

    with admin:
        pending = admin.call("fauna.bridges.list_pending_bridges", {})["bridges"]
        row = _find(pending, bridge_id)
        assert row is not None, "seeded bridge must appear in the pending feed"
        assert row["role"] == "mta"
        assert row["status"] == "pending"

        admin.call(
            "fauna.bridges.approve_pending_bridge",
            {"ed25519_pubkey": pubkey, "role": "mta"},
        )

        pending_after = admin.call("fauna.bridges.list_pending_bridges", {})["bridges"]
        assert _find(pending_after, bridge_id) is None, "approved bridge leaves the pending feed"

        approved = admin.call(
            "fauna.bridges.list_service_users", {"status": "approved"}
        )["service_users"]
        appr = _find(approved, bridge_id)
        assert appr is not None and appr["status"] == "approved"

        # Idempotent: re-approving an approved bridge is ok.
        admin.call(
            "fauna.bridges.approve_pending_bridge",
            {"ed25519_pubkey": pubkey, "role": "mta"},
        )


@pytest.mark.feature("admin-bridges")
def test_reject_flow(dedicated_no_mail_nest):
    """seed pending → reject → it is revoked (and gone from the pending feed)."""
    admin = _admin_client(dedicated_no_mail_nest)
    pubkey, bridge_id = _seed_pending(dedicated_no_mail_nest, "mta")

    with admin:
        admin.call("fauna.bridges.reject_pending_bridge", {"ed25519_pubkey": pubkey})

        pending = admin.call("fauna.bridges.list_pending_bridges", {})["bridges"]
        assert _find(pending, bridge_id) is None, "rejected bridge leaves the pending feed"

        revoked = admin.call(
            "fauna.bridges.list_service_users", {"status": "revoked"}
        )["service_users"]
        assert _find(revoked, bridge_id) is not None, "rejected bridge is revoked"

        # Idempotent: re-rejecting a revoked bridge still succeeds.
        admin.call("fauna.bridges.reject_pending_bridge", {"ed25519_pubkey": pubkey})


def test_approve_role_mismatch_rejected(dedicated_no_mail_nest):
    """Approving an MDA bridge as ``mta`` is refused (role is fixed at enrollment)."""
    admin = _admin_client(dedicated_no_mail_nest)
    pubkey, _ = _seed_pending(dedicated_no_mail_nest, "mda")
    with admin:
        with pytest.raises(RpcCallError) as excinfo:
            admin.call(
                "fauna.bridges.approve_pending_bridge",
                {"ed25519_pubkey": pubkey, "role": "mta"},
            )
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"role mismatch must be refused; got {excinfo.value.code!r}"
    )


def test_approve_unknown_pubkey_not_found(nest_instance):
    """Approving a never-enrolled pubkey is ``not_found``."""
    admin = _admin_client(nest_instance)
    with admin:
        with pytest.raises(RpcCallError) as excinfo:
            admin.call(
                "fauna.bridges.approve_pending_bridge",
                {"ed25519_pubkey": secrets.token_bytes(32), "role": "mta"},
            )
    assert excinfo.value.code == "fauna.bridges.not_found", (
        f"unknown pubkey must be not_found; got {excinfo.value.code!r}"
    )


def test_set_mail_enabled_toggle(nest_instance):
    """The deployment-wide mail-enable toggle dispatches + is admin-gated. The
    flag-file / supervisor-socket effects are unit-tested in the nest
    ``mail_enable`` module; here we prove the kind routes and replies ok."""
    admin = _admin_client(nest_instance)
    with admin:
        assert admin.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok") is True
        assert admin.call("fauna.bridges.set_mail_enabled", {"enabled": False}).get("ok") is True


@pytest.mark.feature("admin-bridges")
def test_non_admin_denied(nest_instance):
    """A User-class actor is rejected by the allowlist on every Stage-1 kind."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        for kind, payload in [
            ("fauna.bridges.list_pending_bridges", {}),
            (
                "fauna.bridges.approve_pending_bridge",
                {"ed25519_pubkey": secrets.token_bytes(32), "role": "mta"},
            ),
            (
                "fauna.bridges.reject_pending_bridge",
                {"ed25519_pubkey": secrets.token_bytes(32)},
            ),
            ("fauna.bridges.set_mail_enabled", {"enabled": True}),
        ]:
            with pytest.raises(RpcCallError) as excinfo:
                client.call(kind, payload)
            assert excinfo.value.code == "fauna.bridges.permission_denied", (
                f"{kind}: User-class must be denied; got {excinfo.value.code!r}"
            )


def _enroll(url, pubkey: bytes, role: str) -> str:
    """Self-enroll a bridge over the real anonymous (pre-identity) WS — the
    loopback-gated ``request_enrollment`` path the Go bridge uses — and return
    the reply status. The nest_instance is on 127.0.0.1, so the connection is
    loopback and the gate passes."""
    with WsRpcAnonClient(url) as anon:
        reply = anon.call(
            "fauna.bridges.request_enrollment",
            {"ed25519_pubkey": pubkey, "role_hint": role, "bridge_id": ""},
        )
    return reply["status"]


@pytest.mark.feature("admin-bridges")
def test_request_enrollment_auto_approves_when_mail_enabled(nest_instance):
    """The box's own bridge self-enrolls over the anonymous (loopback) WS and is
    **auto-approved** once an admin has enabled mail — no manual
    ``approve_pending_bridge`` click (``mail-bridge-lifecycle.md`` § Onboarding
    auto-approval). Exercises the real router + ``request_enrollment`` handler +
    the ``mail_enabled`` DB toggle end-to-end over the socket — coverage the
    in-process handler unit tests (which call the handler directly) cannot give.
    The admin's Admin-class enable is the only thing that opens the window; the
    enrollment itself is the unauthenticated loopback path."""
    admin = _admin_client(nest_instance)
    url = nest_instance["url"]

    # 1. Mail off (fresh nest, toggle unset) → a self-enroll lands pending.
    pk_off = secrets.token_bytes(32)
    assert _enroll(url, pk_off, "mta") == "pending", "mail off → pending"

    try:
        # 2. Admin enables mail — the approval window for the box's own bridges.
        with admin:
            assert (
                admin.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok")
                is True
            )

        # 3. A fresh self-enroll now lands APPROVED with no approve call.
        pk_on = secrets.token_bytes(32)
        assert _enroll(url, pk_on, "mda") == "approved", "mail on → auto-approved"

        # …and it never sat in the pending feed (synthesized bridge_id
        # ``<role>-<pubkey-prefix>``, per request_enrollment_handler).
        synthesized = f"mda-{pk_on.hex()[:8]}"
        with admin:
            pending = admin.call("fauna.bridges.list_pending_bridges", {})["bridges"]
        assert _find(pending, synthesized) is None, (
            "auto-approved bridge must never appear in the pending feed"
        )

        # 4. A bridge that enrolled pending BEFORE the enable self-heals to
        #    approved on its next poll (the existing pk_off re-polls).
        assert _enroll(url, pk_off, "mta") == "approved", (
            "a pre-enable pending bridge self-heals to approved on re-poll"
        )
    finally:
        # Restore the session-scoped nest's default (mail off).
        with admin:
            admin.call("fauna.bridges.set_mail_enabled", {"enabled": False})

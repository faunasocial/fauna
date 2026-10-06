"""E2E test: Self-service onboarding flow.

The admin claims a fresh box and opens registration; Alice then registers with
no further admin intervention, authenticates, checks her quota, resolves her
handle, schedules a handle change, and finally schedules account deletion.

The admin step in front is not incidental — a nest's registration posture is
app-set nest state seeded ``closed`` (``architecture/nest/public-mode.md``
§ Registration modes: "a nest that boots before its admin has picked a posture
admits nobody"), and the ``--registration-open`` flag was deleted with that
ruling on 2026-07-12 along with auto-registration. What "self-service" means is
that Alice needs no admin help *for her own account*, not that a fresh box
admits strangers.

WS-RPC migration note: the personal-account HTTP twins were deleted by the
WS-RPC-everywhere migration (tracked internally, Track B1) and
replaced with WS-RPC kinds, driven here via
``clients.ws_rpc_admin_client.WsRpcAdminClient`` (Admin name is historical — a
User-class actor's keypair makes it a User-class client):

* ``GET /api/v1/quota``            → ``fauna.quota.get``
* ``PUT /api/v1/profile/handle``   → ``fauna.profile.handle.change``
* ``DELETE /api/v1/account``       → ``fauna.account.delete``

Handle change and account deletion are now **deliberately delayed pending
actions** (``bins/fauna-nest/src/pending_actions.rs``: HandleChange = 6h delay,
AccountDelete = 14d delay, both quorum=0), per api-layers.md § Destructive
operations are delayed. So those two steps assert the action is *scheduled* and
that the change has NOT yet taken effect (old handle still resolves, token still
works) — NOT immediate execution. The eventual EXECUTION path (the action
running after its delay) is covered by ``bins/fauna-nest/tests/pending_actions.rs``,
which calls ``execute_ready_actions`` directly (bypassing the wall-clock wait);
an e2e test must not sleep 6h/14d.

Wire contracts verified against ``libs/fauna-protocol/src/account.rs`` +
``bins/fauna-nest/src/account_handlers.rs`` (2026-05-24):
* ``fauna.quota.get`` → ``{tier, inbox:{used_bytes,max_bytes},
  storage:{used_bytes,max_bytes}, devices:{used,max},
  features:{versioned_backup,bridges,max_feeds}}``.
* ``fauna.profile.handle.change`` ``{handle}`` →
  ``{pending_action_id, execute_after, status:"pending", new_handle}``.
* ``fauna.account.delete`` ``{}`` →
  ``{pending_action_id, execute_after, status:"pending", message}``.

Registration (``fauna.account.register``) and the discovery surfaces
(``fauna.nest.info`` / ``fauna.handle.available`` / ``fauna.actor.by_handle``)
are pre-identity WS-RPC kinds — their HTTP twins (``/api/v1/node-info``,
``/api/v1/handle-available/{h}``, ``/api/v1/actor/by-handle/{h}``) were deleted
in the WS-RPC-everywhere cut and are driven here over the anonymous connection
via ``tests.api.ws_api``. No control-plane HTTP twins remain: the auth bootstrap
rides ``fauna.auth.handshake`` (``common.auth.mint_token_via_handshake``) — its
last HTTP twin ``POST /api/v1/auth/token`` was deleted at the rip-out endgame.
"""

import time

from nacl.signing import SigningKey

from common import CLAIM_CODE
from common.auth import mint_token_via_handshake, port_base_url

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient
from tests.api import ws_api

import pytest

pytestmark = pytest.mark.tier_3

DOMAIN = "test.fauna.social"


@pytest.fixture()
def onboarding_nest(request, nest_mode, tmp_path_factory):
    """An UNCLAIMED nest, because the claim ceremony is part of what this test
    witnesses — step 0 below drives it, so the harness must not have done it.

    ``unclaimed`` is the only start option, and every mode declares it, so the
    journey runs unchanged in a container. The nest carries no
    ``handle_domain_seed``: the ``--handle-domain`` boot flag this test used to
    pass is standalone-only, and its replacement is a wire act the test already
    performs — step 0's own claim carries ``mail_domain=DOMAIN``, which
    registers the primary ``mail_domains`` row that becomes the deployment
    identity (``claim_core.rs`` → ``mail_enable::ensure_mail_domain_registered``;
    its sibling ``test_claim_primary_domain.py`` pins that contract, including
    the local-target case this claim used to be). ``testing.md`` § Default app
    and nest mode, ruling (3).
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "onboarding-nest", unclaimed=True)
    yield nest
    cleanup()


def get_auth_token(sk: SigningKey, port: int) -> str:
    """Mint a bearer over the pre-identity WS-RPC ``fauna.auth.handshake`` kind
    (replaces the deleted HTTP twin ``POST /api/v1/auth/token``; same direct-auth
    contract, only the transport moves to the anonymous WS)."""
    return mint_token_via_handshake(port_base_url(port), sk)


# `fauna.account.register` reject code → the legacy HTTP status the retired
# `POST /api/v1/register` twin returned (mirrors `fauna-proxy`'s
# `register_rpc_code_to_status`), so the `(status, data)` callers below are
# unchanged by the WS-RPC migration.
_REGISTER_CODE_STATUS = {
    "fauna.account.handle_taken": 409,
    "fauna.account.actor_exists": 409,
    "fauna.account.registration_closed": 403,
    "fauna.account.invite_required": 403,
    "fauna.account.free_limit_reached": 403,
    "fauna.account.invalid_request": 400,
    "fauna.account.signature_failed": 401,
}


def register_self_service(sk: SigningKey, handle: str, port: int, domain: str):
    """Self-service registration over the pre-identity WS-RPC kind
    ``fauna.account.register`` (the HTTP ``POST /api/v1/register`` twin was
    retired — tracked internally, S4f). Returns ``(status, data)`` shaped
    like the old HTTP twin so callers are unchanged: ``201`` + the reply on
    success, else the mapped reject status + ``{"error": ...}``."""
    actor_id_bytes = bytes(sk.verify_key)
    timestamp = int(time.time() * 1000)
    # The tagged, length-prefixed register message (common.sig_domain).
    from common.sig_domain import register_signed_message

    msg = register_signed_message(actor_id_bytes, handle, domain, timestamp)
    sig = sk.sign(msg).signature

    body = {
        "actor_id": actor_id_bytes.hex(),
        "handle": handle,
        "timestamp": timestamp,
        "signature": sig.hex(),
    }
    try:
        with WsRpcAnonClient(port_base_url(port)) as anon:
            reply = anon.call("fauna.account.register", body)
        return 201, reply
    except RpcCallError as e:
        return _REGISTER_CODE_STATUS.get(e.code, 500), {"error": e.code}


def ws_client(sk: SigningKey, port: int) -> WsRpcAdminClient:
    """Construct a WS-RPC client for the given actor (own challenge/verify auth)."""
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=bytes(sk.verify_key),
        signing_key=bytes(sk),
    )


def _claim_and_open_registration(port: int) -> SigningKey:
    """Claim the nest, then set its registration posture to ``open``.

    ⚠ 2026-08-20: this step did not
    exist, and its absence is why the test failed on a bare
    ``assert False is True``. It was the TEST that encoded a retired premise —
    that a freshly booted nest self-serves registration. It does not, by
    ratified design: ``architecture/nest/public-mode.md`` § Registration modes
    makes the posture app-set nest state whose **pre-claim seed is `closed`**
    ("a nest that boots before its admin has picked a posture admits nobody"),
    the ``--registration-open`` flag was DELETED with that ruling (2026-07-12),
    and auto-registration was removed in the same change. The old comment here
    still said "Start node with open registration" while passing no such flag —
    the flag it referred to had been gone for six weeks.

    So the honest self-service journey now has an admin step in front of it, and
    that is what this test should exercise: the claim ceremony is deliberately
    self-contained (it works on a ``closed`` nest and needs no prior account —
    it is what keeps a fresh box claimable), the admin then opens registration,
    and only then can Alice register with no further admin help. Alice's half —
    everything below step 2 — is unchanged, which is the point: what was retired
    is the *default posture*, not self-service registration.

    The claim also carries ``mail_domain=DOMAIN``, and that is what supplies the
    nest's handle domain — the ``--handle-domain`` boot seed this test used to
    pass is standalone-only (``testing.md`` § Default app and nest mode, ruling
    (3)), while a domained claim is a wire act every nest mode honours. It has
    to be a REAL domain to do anything: the previous ``"localhost"`` was a local
    target, and a local target registers no mail domain at all — its sibling
    ``test_claim_primary_domain.py::test_claim_local_target_registers_no_mail_domain``
    pins exactly that. So the two together are also the reason step 1 can assert
    a domain the harness never told the nest about.
    """
    admin_sk = SigningKey.generate()
    timestamp = int(time.time())
    from common.sig_domain import claim_admin_signed_message

    sig = admin_sk.sign(
        claim_admin_signed_message(bytes(admin_sk.verify_key), timestamp)
    ).signature
    with WsRpcAnonClient(port_base_url(port)) as anon:
        anon.call(
            "fauna.auth.claim_admin",
            {
                "claim_code": CLAIM_CODE,
                "actor_id": bytes(admin_sk.verify_key).hex(),
                "signature": sig.hex(),
                "timestamp": timestamp,
                "handle": "admin",
                # The domained claim IS the handle-domain mechanism here; see
                # the docstring. A local target would register nothing.
                "mail_domain": DOMAIN,
            },
        )
    with ws_client(admin_sk, port) as admin:
        admin.call("fauna.admin.set_registration_mode", {"mode": "open"})
    return admin_sk


@pytest.mark.feature("connect-and-sign-in")
@pytest.mark.feature("account")
def test_alice_self_service_onboarding(onboarding_nest):
    """Alice registers, authenticates, checks quota, resolves handle,
    schedules a handle change, and schedules account deletion — all without
    admin help. Per the WS-RPC migration, the handle change + account deletion
    are delayed pending actions (asserted as *scheduled*, not executed)."""

    port = onboarding_nest["port"]

    # --- Step 0: the admin claims the box and opens registration ---
    # A fresh nest seeds `closed`, so self-service has an admin step in
    # front of it now. Everything from step 2 on is Alice alone, with no
    # further admin help — which is what "self-service" means today.
    _claim_and_open_registration(port)

    # --- Step 1: Query node info ---
    info = ws_api.nest_info(port)
    # Convention 6 — the failure must diagnose itself, and this assertion is now
    # the one that reports whether step 0's DOMAINED CLAIM took: the identity
    # comes from the primary `mail_domains` row that claim registers, not from a
    # boot flag, so a `localhost` here means the claim registered nothing.
    assert info["domain"] == DOMAIN, (
        f"the deployment identity must follow step 0's claimed mail_domain "
        f"{DOMAIN!r}; fauna.nest.info returned domain="
        f"{info.get('domain')!r} (full reply: {info!r})"
    )
    # A bare `assert ... is True` here printed only `assert False is True`, which
    # cost a triage pass: the whole `registration` block
    # is what tells you *why* it is closed (claimed? handle domain unset?).
    status = ws_api.setup_status(port)
    assert status.get("registration_mode") == "open", (
        "the admin opened registration in step 0, so the nest must report the "
        "`open` registration posture; fauna.setup.status returned "
        f"registration_mode={status.get('registration_mode')!r}; nest.info "
        f"registration={info.get('registration')!r}"
    )
    assert info["registration"]["handle_domain"] == DOMAIN, (
        f"registration.handle_domain must echo the claimed domain {DOMAIN!r}; "
        f"got {info['registration'].get('handle_domain')!r}"
    )
    print(f"Node info: domain={info['domain']}, registration_mode={status['registration_mode']}")

    # --- Step 2: Generate Alice's keypair ---
    alice_sk = SigningKey.generate()
    alice_actor_id = bytes(alice_sk.verify_key).hex()
    print(f"Alice actor_id: {alice_actor_id[:16]}...")

    # --- Step 3: Check handle availability ---
    avail = ws_api.handle_available(port, "alice")
    assert avail["available"] is True, (
        "'alice' must be free on a nest with no registrations yet; "
        f"fauna.handle.available returned {avail!r}"
    )
    assert avail["domain"] == DOMAIN, (
        f"handle availability must be scoped to {DOMAIN!r}; got {avail!r}"
    )
    print("Handle 'alice' is available")

    # --- Step 4: Self-service registration ---
    status, data = register_self_service(alice_sk, "alice", port, DOMAIN)
    assert status == 201, f"register failed: {status} {data}"
    assert data["handle"] == "alice"
    assert data["domain"] == DOMAIN
    assert data["tier"] == "free"
    assert data["actor_id"] == alice_actor_id
    print(f"Registered: {data['handle']}@{data['domain']} (tier={data['tier']})")

    # Handle should no longer be available
    avail = ws_api.handle_available(port, "alice")
    assert avail["available"] is False

    # --- Step 5: Authenticate ---
    token = get_auth_token(alice_sk, port)
    assert token, "got empty token"
    print("Authenticated, got bearer token")

    # --- Step 6: Check quota (fauna.quota.get over WS-RPC) ---
    with ws_client(alice_sk, port) as client:
        quota = client.call("fauna.quota.get", {})
    assert quota["tier"] == "free", f"unexpected tier: {quota}"
    # Field shape per QuotaGetReply (account.rs): nested usage objects.
    assert "inbox" in quota and "max_bytes" in quota["inbox"]
    assert "storage" in quota and "max_bytes" in quota["storage"]
    assert "devices" in quota and "max" in quota["devices"]
    assert "features" in quota and "max_feeds" in quota["features"]
    print(f"Quota: tier={quota['tier']}, inbox_max={quota['inbox']['max_bytes']}")

    # --- Step 7: Resolve handle ---
    resolved = ws_api.actor_by_handle(port, "alice")
    assert resolved["actor_id"] == alice_actor_id
    assert resolved["handle"] == "alice"
    assert resolved["domain"] == DOMAIN
    print(f"Resolved handle 'alice' -> {resolved['actor_id'][:16]}...")

    # Unknown handle resolves to fauna.actor.not_found (the twin's 404).
    with pytest.raises(RpcCallError) as excinfo:
        ws_api.actor_by_handle(port, "nobody")
    assert excinfo.value.code == "fauna.actor.not_found"

    # --- Step 8: Schedule a handle change (fauna.profile.handle.change) ---
    # This is now a delayed pending action (6h), NOT an immediate change.
    with ws_client(alice_sk, port) as client:
        change = client.call("fauna.profile.handle.change", {"handle": "alice-v2"})
    assert change["status"] == "pending", f"expected pending, got {change}"
    assert change["new_handle"] == "alice-v2", change
    assert isinstance(change["pending_action_id"], int), change
    # HandleChange delay is 6h; execute_after (epoch seconds) is in the future.
    now = int(time.time())
    assert change["execute_after"] > now, change
    print(f"Handle change scheduled (pending_action_id={change['pending_action_id']})")

    # The handle has NOT changed yet — alice still resolves, alice-v2 does not.
    resolved = ws_api.actor_by_handle(port, "alice")
    assert resolved["actor_id"] == alice_actor_id, (
        "old handle should still resolve (change is only scheduled)"
    )
    with pytest.raises(RpcCallError) as excinfo:
        ws_api.actor_by_handle(port, "alice-v2")
    assert excinfo.value.code == "fauna.actor.not_found", (
        "new handle must not resolve until the pending action executes"
    )

    # Alice can see the scheduled change via fauna.pending_actions.list.
    with ws_client(alice_sk, port) as client:
        listed = client.call("fauna.pending_actions.list", {})
    types = [a["action_type"] for a in listed["actions"]]
    assert "handle.change" in types, f"handle.change not in {types}"
    print("Pending handle-change visible in actor's pending-actions list")

    # --- Step 9: Duplicate registration should fail ---
    status, _ = register_self_service(alice_sk, "alice-dup", port, DOMAIN)
    assert status == 409, f"expected 409 for duplicate registration, got {status}"
    print("Duplicate registration correctly rejected (409)")

    # --- Step 10: Schedule account deletion (fauna.account.delete) ---
    # Now a delayed pending action (14d), NOT immediate deletion.
    with ws_client(alice_sk, port) as client:
        deletion = client.call("fauna.account.delete", {})
    assert deletion["status"] == "pending", f"expected pending, got {deletion}"
    assert isinstance(deletion["pending_action_id"], int), deletion
    assert deletion["message"], "expected a confirmation message"
    now = int(time.time())
    assert deletion["execute_after"] > now, deletion
    print(f"Account deletion scheduled (pending_action_id={deletion['pending_action_id']})")

    # The account is NOT yet deleted — handle still resolves, token still works.
    resolved = ws_api.actor_by_handle(port, "alice")
    assert resolved["actor_id"] == alice_actor_id, (
        "handle should still resolve (deletion only scheduled)"
    )
    with ws_client(alice_sk, port) as client:
        quota_after = client.call("fauna.quota.get", {})
    assert quota_after["tier"] == "free", (
        "account still active until the deletion pending action executes"
    )
    print("Account still active (deletion only scheduled, not executed)")

    print("\n=== Alice's self-service onboarding flow: ALL PASSED ===")


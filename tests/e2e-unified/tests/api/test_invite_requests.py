"""In-band invite-request endpoints — now fully WS-RPC.

The user-side ceremonies migrated to the pre-identity WS-RPC kinds (the HTTP
twins were retired in S4a2):

  fauna.account.invite_request.submit   (≡ POST   /api/v1/invite-requests)
  fauna.account.invite_request.status   (≡ GET    /api/v1/invite-requests/<actor>/status)
  fauna.account.invite_request.cancel   (≡ DELETE /api/v1/invite-requests/<actor>)

They are driven here over the anonymous WS-RPC client (no bearer) — the same
pre-identity connector the onboarding wizard rides. The signed-body contracts
(hex actor_id, u64 ms timestamp, hex Ed25519 signature) are byte-identical to the
retired HTTP twins; see `bins/fauna-nest/src/invite_core.rs`.

The admin decision endpoints likewise migrated to WS-RPC (the deprecated
`/admin/api/invite-requests*` HTTP twins were deleted in the WS-RPC-everywhere
rip-out):

  fauna.admin.invite_requests.list      (≡ GET  /admin/api/invite-requests)
  fauna.admin.invite_requests.approve   (≡ POST /admin/api/invite-requests/<id>/approve)
  fauna.admin.invite_requests.deny      (≡ POST /admin/api/invite-requests/<id>/deny)

These are **Admin-class** (`bridge_method_allowlist.rs`), reached over the same
canonical WS-RPC wire as every other admin kind via the nest's claimed admin
keypair (`clients.ws_rpc_admin_client.WsRpcAdminClient`). `approve`'s reply is
`{actor_id, handle, tier}` (raw 32-byte `actor_id`, no `ok` field — success is the
`ok=true` envelope); a missing request is `fauna.admin.not_found`, a non-pending
request `fauna.admin.conflict` (`admin_ws_handlers.rs`).

`test_admin_approve_creates_user` proves the approved actor can sign in via the
pre-identity WS-RPC kinds `fauna.auth.challenge` / `fauna.auth.verify` (over the
`anon` connection); their `/api/v1/auth/{challenge,verify}` HTTP twins were
deleted in the WS-RPC-everywhere rip-out. No HTTP endpoint is exercised here.
"""

import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_anon_client import WsRpcAnonClient, RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from tests.api import ws_api

pytestmark = pytest.mark.tier_3


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _admin_client(nest) -> WsRpcAdminClient:
    """An Admin-class WS-RPC client for the nest's claimed admin.

    Mirrors the sibling `test_services.py::_admin_client` — one canonical wire
    surface for every admin kind, driven by the admin's Ed25519 keypair.
    """
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _new_keypair():
    sk = SigningKey.generate()
    return sk, bytes(sk.verify_key).hex()


def _sign_submit(sk, actor_hex, handle, message, timestamp_ms):
    """Sign the tagged, length-prefixed invite-submit message (common.sig_domain)."""
    from common.sig_domain import invite_submit_signed_message

    msg = invite_submit_signed_message(bytes.fromhex(actor_hex), handle, message, timestamp_ms)
    return sk.sign(msg).signature.hex()


def _sign_cancel(sk, actor_hex, timestamp_ms):
    """Sign the tagged invite-cancel message (common.sig_domain)."""
    from common.sig_domain import invite_cancel_signed_message

    msg = invite_cancel_signed_message(bytes.fromhex(actor_hex), timestamp_ms)
    return sk.sign(msg).signature.hex()


def _submit(anon, sk, actor_hex, handle="newuser", message=""):
    """fauna.account.invite_request.submit — returns the reply body dict.

    Raises RpcCallError on a rejection.
    """
    ts = int(time.time() * 1000)
    sig = _sign_submit(sk, actor_hex, handle, message, ts)
    return anon.call(
        "fauna.account.invite_request.submit",
        {
            "actor_id": actor_hex,
            "handle": handle,
            "message": message,
            "timestamp": ts,
            "signature": sig,
        },
    )


def _status(anon, actor_hex):
    return anon.call("fauna.account.invite_request.status", {"actor_id": actor_hex})


# ---------------------------------------------------------------------------
# Fixtures: fresh nest per test (admin approvals mutate global state) + a
# connected anonymous WS-RPC client the user ceremonies ride.
# ---------------------------------------------------------------------------


@pytest.fixture()
def nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    info, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "invite_requests")
    yield info
    cleanup()


@pytest.fixture()
def anon(nest):
    """A connected anonymous (pre-identity) WS-RPC client for the test's nest.

    The invite-request user ceremonies (submit/status/cancel) ride the bare
    `fauna.v1` anonymous connection — the same pre-identity connector the
    onboarding wizard uses. Yielding an already-`__enter__`ed client keeps each
    test body free of `with WsRpcAnonClient(...)` boilerplate while still closing
    the socket on teardown.
    """
    with WsRpcAnonClient(nest["url"]) as c:
        yield c


# ---------------------------------------------------------------------------
# fauna.account.invite_request.submit
# ---------------------------------------------------------------------------


@pytest.mark.feature("join-a-nest")
def test_submit_happy_path(nest, anon):
    sk, actor_hex = _new_keypair()
    data = _submit(anon, sk, actor_hex, handle="alice", message="let me in")
    assert data["status"] == "pending"
    assert data["handle"] == "alice"
    assert data["message"] == "let me in"
    assert data["actor_id"] == actor_hex
    assert isinstance(data["id"], int)


@pytest.mark.feature("join-a-nest")
def test_submit_bad_signature(nest, anon):
    sk, actor_hex = _new_keypair()
    other_sk, _ = _new_keypair()
    ts = int(time.time() * 1000)
    bad_sig = _sign_submit(other_sk, actor_hex, "bob", "", ts)
    with pytest.raises(RpcCallError) as ei:
        anon.call(
            "fauna.account.invite_request.submit",
            {
                "actor_id": actor_hex,
                "handle": "bob",
                "message": "",
                "timestamp": ts,
                "signature": bad_sig,
            },
        )
    assert ei.value.code == "fauna.account.signature_failed"


def test_submit_stale_timestamp(nest, anon):
    sk, actor_hex = _new_keypair()
    ts = int(time.time() * 1000) - 120_000  # 2 minutes ago
    sig = _sign_submit(sk, actor_hex, "cheryl", "", ts)
    with pytest.raises(RpcCallError) as ei:
        anon.call(
            "fauna.account.invite_request.submit",
            {
                "actor_id": actor_hex,
                "handle": "cheryl",
                "message": "",
                "timestamp": ts,
                "signature": sig,
            },
        )
    assert ei.value.code == "fauna.account.invalid_request"


def test_submit_invalid_handle(nest, anon):
    sk, actor_hex = _new_keypair()
    with pytest.raises(RpcCallError) as ei:
        _submit(anon, sk, actor_hex, handle="x")  # too short
    assert ei.value.code == "fauna.account.invalid_request"


def test_submit_duplicate_actor_conflict(nest, anon):
    sk, actor_hex = _new_keypair()
    assert _submit(anon, sk, actor_hex, handle="dave")["status"] == "pending"
    with pytest.raises(RpcCallError) as ei:
        _submit(anon, sk, actor_hex, handle="daveagain")
    assert ei.value.code == "fauna.account.invite_request_exists"
    # The WS error (unlike the HTTP twin's 409 body) drops the existing row; the
    # caller re-queries status, which still carries the original handle.
    assert _status(anon, actor_hex)["handle"] == "dave"


def test_submit_conflict_on_taken_handle(nest, anon):
    """Cannot request a handle someone already holds."""
    # Submit → admin approve → second submitter wants the same handle.
    sk1, actor1 = _new_keypair()
    req_id = _submit(anon, sk1, actor1, handle="claimed")["id"]

    with _admin_client(nest) as admin:
        admin.call("fauna.admin.invite_requests.approve", {"id": req_id})

    sk2, actor2 = _new_keypair()
    with pytest.raises(RpcCallError) as ei:
        _submit(anon, sk2, actor2, handle="claimed")
    assert ei.value.code == "fauna.account.handle_taken"


# ---------------------------------------------------------------------------
# fauna.account.invite_request.status
# ---------------------------------------------------------------------------


def test_status_pending(nest, anon):
    sk, actor_hex = _new_keypair()
    _submit(anon, sk, actor_hex, handle="eve", message="hi")
    data = _status(anon, actor_hex)
    assert data["status"] == "pending"
    assert data["handle"] == "eve"


def test_status_not_found(nest, anon):
    _, actor_hex = _new_keypair()
    with pytest.raises(RpcCallError) as ei:
        _status(anon, actor_hex)
    assert ei.value.code == "fauna.account.invite_request_not_found"


def test_status_bad_actor(nest, anon):
    with pytest.raises(RpcCallError) as ei:
        _status(anon, "not-hex")
    assert ei.value.code == "fauna.account.invalid_request"


# ---------------------------------------------------------------------------
# fauna.account.invite_request.cancel
# ---------------------------------------------------------------------------


def test_cancel_happy_path(nest, anon):
    sk, actor_hex = _new_keypair()
    _submit(anon, sk, actor_hex, handle="frank")

    ts = int(time.time() * 1000)
    sig = _sign_cancel(sk, actor_hex, ts)
    reply = anon.call(
        "fauna.account.invite_request.cancel",
        {"actor_id": actor_hex, "timestamp": ts, "signature": sig},
    )
    assert reply["ok"] is True

    # Status now reports not-found.
    with pytest.raises(RpcCallError) as ei:
        _status(anon, actor_hex)
    assert ei.value.code == "fauna.account.invite_request_not_found"


def test_cancel_bad_signature(nest, anon):
    sk, actor_hex = _new_keypair()
    _submit(anon, sk, actor_hex, handle="grace")

    other_sk, _ = _new_keypair()
    ts = int(time.time() * 1000)
    sig = _sign_cancel(other_sk, actor_hex, ts)  # wrong key
    with pytest.raises(RpcCallError) as ei:
        anon.call(
            "fauna.account.invite_request.cancel",
            {"actor_id": actor_hex, "timestamp": ts, "signature": sig},
        )
    assert ei.value.code == "fauna.account.signature_failed"


def test_cancel_nothing_to_cancel(nest, anon):
    sk, actor_hex = _new_keypair()
    ts = int(time.time() * 1000)
    sig = _sign_cancel(sk, actor_hex, ts)
    with pytest.raises(RpcCallError) as ei:
        anon.call(
            "fauna.account.invite_request.cancel",
            {"actor_id": actor_hex, "timestamp": ts, "signature": sig},
        )
    assert ei.value.code == "fauna.account.invite_request_not_found"


# ---------------------------------------------------------------------------
# Admin decision kinds (fauna.admin.invite_requests.{list,approve,deny})
# ---------------------------------------------------------------------------


def test_admin_list_requires_admin(nest):
    """A non-admin User-class caller cannot list invite requests.

    WS-RPC successor to the twin's "no bearer → 401": the WS handshake is
    signature-authed (there is no unauthenticated call), so the meaningful
    negative is the caller-class gate — a User-class actor invoking the
    Admin-only kind is rejected with `fauna.admin.permission_denied`
    (`admin_ws_handlers::require_permission`), mirroring
    `test_services.py::test_non_admin_caller_is_denied`.
    """
    user = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    user_client = WsRpcAdminClient(
        nest["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )
    with user_client as c:
        with pytest.raises(RpcCallError) as ei:
            c.call("fauna.admin.invite_requests.list", {})
    assert ei.value.code == "fauna.admin.permission_denied"


@pytest.mark.feature("admin-users")
def test_admin_list_shows_submitted(nest, anon):
    sk_a, actor_a = _new_keypair()
    sk_b, actor_b = _new_keypair()
    _submit(anon, sk_a, actor_a, handle="harry")
    _submit(anon, sk_b, actor_b, handle="ivy")

    with _admin_client(nest) as admin:
        data = admin.call("fauna.admin.invite_requests.list", {})
    handles = {r["handle"] for r in data["invite_requests"]}
    assert {"harry", "ivy"}.issubset(handles)


@pytest.mark.feature("join-a-nest")
def test_admin_approve_creates_user(nest, anon):
    """After approve, the actor is registered and challenge-response signs them in."""
    sk, actor_hex = _new_keypair()
    req_id = _submit(anon, sk, actor_hex, handle="janice")["id"]

    with _admin_client(nest) as admin:
        data = admin.call("fauna.admin.invite_requests.approve", {"id": req_id})
    # WS reply is {actor_id, handle, tier} — no `ok` (success is the ok=true
    # envelope); actor_id rides as raw 32 bytes, not the twin's hex string.
    assert data["actor_id"].hex() == actor_hex
    assert data["handle"] == "janice"
    assert data["tier"] == "free"

    # Request row is gone (WS status reports not-found).
    with pytest.raises(RpcCallError) as ei:
        _status(anon, actor_hex)
    assert ei.value.code == "fauna.account.invite_request_not_found"

    # Handle now resolves to the new user (fauna.actor.by_handle; HTTP twin deleted).
    resolved = ws_api.actor_by_handle(nest["port"], "janice")
    assert resolved["actor_id"] == actor_hex

    # Silent-challenge sign-in works over the pre-identity WS-RPC kinds
    # (fauna.auth.challenge / fauna.auth.verify; the HTTP twins were deleted in
    # the WS-RPC-everywhere rip-out). Reuse the in-scope anonymous connection.
    from common.nest_identity import read_nest_identity
    from common.sig_domain import challenge_verify_signed_message

    nest_id = read_nest_identity(anon)
    chal = anon.call("fauna.auth.challenge", {"actor_id": actor_hex})
    nonce_bytes = bytes.fromhex(chal["nonce"])
    sig = sk.sign(
        challenge_verify_signed_message(bytes.fromhex(actor_hex), nonce_bytes, nest_id)
    ).signature.hex()
    verify = anon.call(
        "fauna.auth.verify",
        {"actor_id": actor_hex, "nonce": chal["nonce"], "signature": sig, "nest_id": nest_id.hex()},
    )
    assert verify["handle"] == "janice"
    assert "token" in verify
    assert "token" in verify


def test_admin_approve_with_tier(nest, anon):
    sk, actor_hex = _new_keypair()
    req_id = _submit(anon, sk, actor_hex, handle="kim")["id"]

    with _admin_client(nest) as admin:
        data = admin.call(
            "fauna.admin.invite_requests.approve",
            {"id": req_id, "tier": "personal", "label": "VIP"},
        )
    assert data["tier"] == "personal"


def test_admin_approve_unknown_id(nest):
    with _admin_client(nest) as admin:
        with pytest.raises(RpcCallError) as ei:
            admin.call("fauna.admin.invite_requests.approve", {"id": 999999})
    assert ei.value.code == "fauna.admin.not_found"


@pytest.mark.feature("join-a-nest")
def test_admin_deny_marks_denied(nest, anon):
    sk, actor_hex = _new_keypair()
    req_id = _submit(anon, sk, actor_hex, handle="leo")["id"]

    with _admin_client(nest) as admin:
        admin.call(
            "fauna.admin.invite_requests.deny",
            {"id": req_id, "reason": "not a match"},
        )

    # User polls and sees denied status (over WS).
    data = _status(anon, actor_hex)
    assert data["status"] == "denied"
    assert data["denial_reason"] == "not a match"


def test_admin_approve_twice_fails(nest, anon):
    sk, actor_hex = _new_keypair()
    req_id = _submit(anon, sk, actor_hex, handle="mia")["id"]

    with _admin_client(nest) as admin:
        admin.call("fauna.admin.invite_requests.approve", {"id": req_id})
        # Second approve: row is gone.
        with pytest.raises(RpcCallError) as ei:
            admin.call("fauna.admin.invite_requests.approve", {"id": req_id})
    assert ei.value.code == "fauna.admin.not_found"


def test_admin_deny_then_approve_conflict(nest, anon):
    sk, actor_hex = _new_keypair()
    req_id = _submit(anon, sk, actor_hex, handle="nora")["id"]

    with _admin_client(nest) as admin:
        admin.call("fauna.admin.invite_requests.deny", {"id": req_id})
        # Approve on denied row: not pending anymore.
        with pytest.raises(RpcCallError) as ei:
            admin.call("fauna.admin.invite_requests.approve", {"id": req_id})
    assert ei.value.code == "fauna.admin.conflict"

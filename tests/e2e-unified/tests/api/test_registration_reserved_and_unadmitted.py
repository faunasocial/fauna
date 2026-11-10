"""Which handles nobody holds, and that a key alone is never an account —
witnessed at the wire (`docs/goal/architecture/nest/public-mode.md`
§ Registration & Identity).

* **join-a-nest 13** — a handle on the nest's reserved list (admin, www, mail,
  relay, mta-sts, …: the hard-coded `fauna_protocol::handle::reserved_handles()`
  no admin can configure) is refused at sign-up and as a later handle change,
  with registration open and the admin's own mail reserved list saved empty —
  so no admin setting frees it.
* **join-a-nest 14** — auto-registration is gone: a key the nest never admitted
  is refused sign-in (`fauna.auth.not_registered`) in every registration mode,
  open included, and the refusal leaves no `users` row behind.

Each refusal is a two-way discriminator: the same call with the one fact the
rule keys on changed (an unreserved handle, an admitted key) goes through, so
"refused" cannot be told from "this call never works". The module runs on a
nest of its own: its registers stay off `test_registration_posture.py`'s
shared `fauna.account.register` budget (10 per source per minute).
"""

from __future__ import annotations

import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import (
    create_actor_and_register,
    handshake_via_ws,
    set_registration_mode,
)
from common.sig_domain import register_signed_message

pytestmark = pytest.mark.tier_3

NEST_DOMAIN = "reserved.example"

# The five the outcome names; the full list is the Rust constant's.
RESERVED = ("admin", "www", "mail", "relay", "mta-sts")
MODES = ("open", "invite_required", "closed")

REGISTER_INVALID = "fauna.account.invalid_request"
PROFILE_INVALID = "fauna.profile.invalid_request"
NOT_REGISTERED = "fauna.auth.not_registered"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture(scope="module")
def reserved_nest(request, nest_mode, tmp_path_factory):
    """A nest claimed onto ``NEST_DOMAIN``, the domain every register
    signature is over."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reserved-nest", claim_domain=NEST_DOMAIN,
    )
    nest["admin_sk"] = _as_signing_key(nest["admin"]["signing_key"])
    yield nest
    cleanup()


def _admin(nest) -> WsRpcAdminClient:
    sk = nest["admin_sk"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _mode(nest, mode: str) -> None:
    set_registration_mode(nest["port"], base_url=nest["url"],
                          admin_signing_key=nest["admin_sk"], mode=mode)


def _handle(prefix: str) -> str:
    return f"{prefix}{secrets.token_hex(3)}"


def _register(nest, handle: str) -> SigningKey:
    """`fauna.account.register` for a fresh key; raises `RpcCallError` on a
    refusal."""
    sk = SigningKey.generate()
    actor = bytes(sk.verify_key)
    ts = int(time.time() * 1000)
    body = {
        "actor_id": actor.hex(),
        "handle": handle,
        "timestamp": ts,
        "signature": sk.sign(register_signed_message(actor, handle, NEST_DOMAIN, ts)).signature.hex(),
    }
    with WsRpcAnonClient(nest["url"]) as anon:
        anon.call("fauna.account.register", body)
    return sk


def _refused(fn, *args) -> RpcCallError:
    with pytest.raises(RpcCallError) as ei:
        fn(*args)
    return ei.value


def _actor_ids(nest) -> set[bytes]:
    with _admin(nest) as admin:
        return {bytes(u["actor_id"]) for u in admin.users_list_all()}


# ── join-a-nest 13 ───────────────────────────────────────────────────────────


@pytest.mark.feature("join-a-nest")
def test_a_reserved_handle_is_refused_at_sign_up_and_as_a_handle_change(reserved_nest):
    """Outcome 13: with registration open and the admin's reserved list saved
    empty, every reserved handle is refused as a sign-up and as an admitted
    member's handle change; an unreserved handle goes through both ways."""
    nest = reserved_nest
    _mode(nest, "open")
    with _admin(nest) as admin:
        admin.call("fauna.bridges.put_alias_policy", {"reserved_local_parts": []})
    try:
        with _admin(nest) as admin:
            shown = admin.call("fauna.bridges.get_alias_policy", {})
        assert shown["reserved_local_parts"] == [], (
            f"the admin's reserved list must read back empty: {shown!r}"
        )

        before = _actor_ids(nest)
        for handle in RESERVED:
            refused = _refused(_register, nest, handle)
            assert refused.code == REGISTER_INVALID and "reserved" in str(refused.details), (
                f"signing up as {handle!r} must be refused as reserved; got "
                f"{refused.code} {refused.details!r}"
            )
        assert _actor_ids(nest) == before, "a refused sign-up must create no account"
        _register(nest, _handle("free"))  # the discriminator: open mode admits

        actor = create_actor_and_register(
            nest["port"], base_url=nest["url"], admin_signing_key=nest["admin_sk"],
        )
        member = SigningKey(bytes(actor["signing_key"]))
        with WsRpcAdminClient(nest["url"], actor_id=bytes(member.verify_key),
                              signing_key=bytes(member)) as ws:
            for handle in RESERVED:
                refused = _refused(ws.call, "fauna.profile.handle.change", {"handle": handle})
                assert refused.code == PROFILE_INVALID and "reserved" in str(refused.details), (
                    f"changing a handle to {handle!r} must be refused as reserved; "
                    f"got {refused.code} {refused.details!r}"
                )
            # The discriminator: the same member's change to an unreserved
            # handle goes through.
            ws.call("fauna.profile.handle.change", {"handle": _handle("renamed")})
    finally:
        with _admin(nest) as admin:
            admin.call("fauna.bridges.put_alias_policy", {})


# ── join-a-nest 14 ───────────────────────────────────────────────────────────


@pytest.mark.feature("join-a-nest")
@pytest.mark.parametrize("mode", MODES)
def test_a_key_the_nest_never_admitted_is_refused_sign_in_whatever_the_mode(
    reserved_nest, mode,
):
    """Outcome 14: holding a key is never an account. A fresh key with no
    `users` row is refused `fauna.auth.not_registered` in every registration
    mode, open included, and no account appears; an admitted key signs in."""
    nest = reserved_nest
    _mode(nest, mode)
    stranger = SigningKey.generate()

    refused = _refused(handshake_via_ws, nest["url"], stranger)
    assert refused.code == NOT_REGISTERED, (
        f"under {mode!r} a never-admitted key must be refused as not registered; "
        f"got {refused.code} {refused.details!r}"
    )
    assert bytes(stranger.verify_key) not in _actor_ids(nest), (
        f"under {mode!r} the refused sign-in must leave no account behind"
    )

    # The discriminator: an admitted key signs in under the same mode.
    actor = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin_sk"],
    )
    assert handshake_via_ws(nest["url"], SigningKey(bytes(actor["signing_key"])))["token"]

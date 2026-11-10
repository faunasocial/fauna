"""tier_3 API e2e: a handle and an alias never hold the same address.

`docs/features/mail-aliases.md` outcome 30, observed from outside over the real
WS-RPC wire (`docs/goal/behavior/mail-aliases.md` § Cross-actor isolation +
reserved local-parts → *Cross-user uniqueness*, the 2026-10-14 ruling): a
registered handle is its actor's address on every active local domain, so

* **the alias side** — an alias create, and the admin's forwarder and a list
  creator, refuse a local part that is another account's handle (a forwarder or
  a list on the creator's own handle too) with
  `fauna.bridges.conflicts_with_existing_alias`; the one row a handle tolerates
  on its local part is its own actor's exact alias, the canonical address;
* **the handle side** — every handle door refuses a handle an exact alias,
  forwarder or list already holds, as "handle taken": the admin's account
  create, open self-registration, an invite request and its approval (which
  re-checks, because the alias may have been made while the request waited),
  and the handle change.

The admin claim is a handle door too (`claim_core::handle_write_err`), but it
cannot be reached from outside with the precondition in place: no alias,
forwarder or list can exist on a nest before its first account — the claimer —
does. It stays pinned in-process.

The handle side runs on a nest this module owns: it opens registration, which
the session's shared nest must never carry into other tests.
"""

from __future__ import annotations

import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import create_actor_and_register, open_registration

pytestmark = pytest.mark.tier_3

#: The alias-side domains: alias and list creates key on any local domain
#: (the FK is loose), a forwarder needs a hosted one, added idempotently.
ALIAS_DOMAIN = "handle-exclusion.test"
SECOND_DOMAIN = "handle-exclusion-second.test"
#: The dedicated nest's identity domain (a registration is signed over it).
DOORS_DOMAIN = "handle-doors.example"

CONFLICT = "fauna.bridges.conflicts_with_existing_alias"


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"]),
    )


def _admin_ws(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _fresh_actor(nest) -> dict:
    """A registered actor with a handle (`e2e-<12 hex>`), nobody else's."""
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"],
    )


def _add_hosted_domain(admin, domain) -> None:
    admin.call("fauna.bridges.add_local_domain",
               {"domain": domain, "mta_sts_cert_mode": "expand_primary"})


def _create_alias(client, domain, pattern):
    return client.call("fauna.bridges.create_account_alias", {
        "kind": "exact", "local_domain": domain, "pattern": pattern,
        "controls": {"label": ""},
    })


def _create_list(client, domain, local_part):
    return client.call("fauna.bridges.create_account_list",
                       {"local_part": local_part, "local_domain": domain})


def _create_forwarder(admin, domain, pattern):
    return admin.call("fauna.bridges.create_forwarder", {
        "local_domain": domain, "pattern": pattern,
        "forward_target": f"{pattern}@external.example",
    })


def _refused(code: str, what: str, call) -> RpcCallError:
    with pytest.raises(RpcCallError) as refused:
        call()
    assert refused.value.code == code, f"{what} must be refused {code}; got {refused.value!r}"
    return refused.value


@pytest.mark.feature("mail-aliases")
def test_no_alias_forwarder_or_list_can_take_a_handle(nest_instance):
    """Another account's alias, list or forwarder on a handle is refused on any
    local domain; the handle's own actor may not put a list or forwarder there
    either, while its own exact alias — the canonical address — is the one row
    the handle tolerates."""
    nest = nest_instance
    owner = _fresh_actor(nest)
    other = _fresh_actor(nest)
    handle = owner["handle"]
    assert handle, "the harness admits every actor under a handle"

    with _ws(nest, other) as ws:
        for domain in (ALIAS_DOMAIN, SECOND_DOMAIN):
            _refused(CONFLICT, f"another account's alias on the handle at {domain}",
                     lambda: _create_alias(ws, domain, handle))
        _refused(CONFLICT, "another account's list on the handle",
                 lambda: _create_list(ws, ALIAS_DOMAIN, handle))
        # Control: the same account may claim a free neighbour of the handle.
        _create_alias(ws, ALIAS_DOMAIN, f"{handle}-x")

    with _admin_ws(nest) as admin:
        _add_hosted_domain(admin, ALIAS_DOMAIN)
        _refused(CONFLICT, "an admin forwarder on a member's handle",
                 lambda: _create_forwarder(admin, ALIAS_DOMAIN, handle))

    with _ws(nest, owner) as ws:
        _refused(CONFLICT, "a list on the creator's own handle",
                 lambda: _create_list(ws, ALIAS_DOMAIN, handle))
        # The one tolerated row: the handle's own exact alias.
        _create_alias(ws, ALIAS_DOMAIN, handle)
        rows = [r for r in ws.call("fauna.bridges.list_account_aliases", {})["aliases"]
                if r["pattern"] == handle]
    assert len(rows) == 1 and rows[0]["actor_id"] == owner["actor_id_bytes"], (
        f"the handle's own exact alias must be written for its owner; rows={rows}")


@pytest.fixture()
def handle_doors_nest(request, nest_mode, tmp_path_factory):
    """A nest of this module's own, claimed under a domain (registration and an
    invite request are signed over its identity), with registration opened."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "handle-alias-doors-nest",
        claim_domain=DOORS_DOMAIN,
    )
    try:
        open_registration(nest)
        yield nest
    finally:
        cleanup()


def _register(nest, domain: str, handle: str):
    from common.sig_domain import register_signed_message

    sk = SigningKey.generate()
    actor_id = bytes(sk.verify_key)
    ts_ms = int(time.time() * 1000)
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.account.register", {
            "actor_id": actor_id.hex(), "handle": handle, "timestamp": ts_ms,
            "signature": sk.sign(
                register_signed_message(actor_id, handle, domain, ts_ms)).signature.hex(),
        })


def _submit_invite(nest, sk: SigningKey, handle: str):
    from common.sig_domain import invite_submit_signed_message

    actor_id = bytes(sk.verify_key)
    message = "let me in"
    ts_ms = int(time.time() * 1000)
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.account.invite_request.submit", {
            "actor_id": actor_id.hex(), "handle": handle, "message": message,
            "timestamp": ts_ms,
            "signature": sk.sign(invite_submit_signed_message(
                actor_id, handle, message, ts_ms)).signature.hex(),
        })


@pytest.mark.feature("mail-aliases")
def test_every_handle_door_refuses_a_handle_an_alias_holds(handle_doors_nest):
    """A handle an exact alias, a forwarder or a list holds is "handle taken" at
    the admin's account create, open registration, an invite request and the
    handle change; an invite approved after an alias took its handle is refused
    at the approval. The control: an account may still take its OWN exact
    alias's address as its handle."""
    from tests.api import ws_api

    nest = handle_doors_nest
    domain = ws_api.nest_info(nest["port"])["domain"]
    assert domain == DOORS_DOMAIN, f"the claim must give the nest its domain: {domain!r}"
    tag = secrets.token_hex(3)
    holder = _fresh_actor(nest)
    changer = _fresh_actor(nest)

    held = {"exact alias": f"held-{tag}", "forwarder": f"fwd-{tag}", "list": f"lst-{tag}"}
    with _ws(nest, holder) as ws:
        _create_alias(ws, domain, held["exact alias"])
        _create_list(ws, domain, held["list"])
    with _admin_ws(nest) as admin:
        _add_hosted_domain(admin, domain)
        _create_forwarder(admin, domain, held["forwarder"])

    for kind, name in held.items():
        with _admin_ws(nest) as admin:
            _refused("fauna.admin.conflict", f"the admin's account create on a {kind}'s address",
                     lambda: admin.call("fauna.admin.users.create", {
                         "actor_id": bytes(SigningKey.generate().verify_key),
                         "tier": "free", "label": "door", "handle": name,
                     }))
        _refused("fauna.account.handle_taken", f"open registration on a {kind}'s address",
                 lambda: _register(nest, domain, name))
        _refused("fauna.account.handle_taken", f"an invite request on a {kind}'s address",
                 lambda: _submit_invite(nest, SigningKey.generate(), name))
        with _ws(nest, changer) as ws:
            _refused("fauna.profile.handle_taken", f"a handle change to a {kind}'s address",
                     lambda: ws.call("fauna.profile.handle.change", {"handle": name}))

    # The approval re-checks: the request is filed while the handle is free,
    # an alias takes the address while it waits, and approving it is refused.
    late = f"late-{tag}"
    applicant = SigningKey.generate()
    _submit_invite(nest, applicant, late)
    with _ws(nest, holder) as ws:
        _create_alias(ws, domain, late)
    with _admin_ws(nest) as admin:
        pending = [r for r in admin.call("fauna.admin.invite_requests.list", {})["invite_requests"]
                   if bytes(r["actor_id"]) == bytes(applicant.verify_key)]
        assert len(pending) == 1, f"the invite request must be pending: {pending!r}"
        _refused("fauna.admin.conflict", "approving an invite whose handle an alias took",
                 lambda: admin.call("fauna.admin.invite_requests.approve",
                                    {"id": pending[0]["id"]}))

    # Control: the holder's own exact alias does not block the holder.
    with _ws(nest, holder) as ws:
        ws.call("fauna.profile.handle.change", {"handle": held["exact alias"]})

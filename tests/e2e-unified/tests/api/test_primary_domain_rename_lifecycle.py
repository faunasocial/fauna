"""tier_3 API e2e: the nest's half of a primary-domain rename that the
Domains page relies on before the anchor flip — what an unfinished rename
refuses (``mail-primary-domain-rename.md`` § RPC refusal codes).

Each test runs on its OWN nest (``_start_dedicated_nest``): starting a rename
is a state change the session-shared ``nest_instance`` must never see (that
file, ``tests/test_primary_domain_rename.py``, stays side-effect-free).

A harness nest has no certificate authority to order from, so a rename it
starts rests in ``cert_issuance`` and never flips; the post-flip outcomes
(grace, abort from grace, complete, what members and signups see afterwards)
are witnessed against a real test CA in
``tests/platform/docker/test_primary_domain_rename_flip.py``.

Wire contracts: ``fauna.bridges.{start,abort}_primary_domain_rename`` and
``remove_local_domain`` — ``libs/fauna-protocol/src/bridge_routing.rs``; the
``domain_in_rename_flight`` refusal —
``bins/fauna-nest/src/bridge_routing_handlers.rs::domain_in_rename_flight``.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from helpers.local_domains import (
    active_domains,
    add_local_domain,
    ensure_primary,
    set_host_address,
)

pytestmark = pytest.mark.tier_3


@pytest.fixture
def rename_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "domain-rename-nest")
    try:
        yield nest
    finally:
        cleanup()


def _admin(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _start(admin, new_domain_id: bytes) -> dict:
    return admin.call("fauna.bridges.start_primary_domain_rename", {
        "new_primary_domain_id": new_domain_id,
        "grace_days": 7,
    })["rename"]


@pytest.mark.feature("admin-dns-and-certificates")
def test_neither_domain_of_an_unfinished_rename_can_be_removed(rename_nest):
    """Outcome 49: while a rename is unfinished, removing either its old or its
    new domain is refused ``domain_in_rename_flight`` and removes nothing;
    once the rename is aborted the new domain can be removed again."""
    with _admin(rename_nest) as admin:
        set_host_address(admin)
        old = ensure_primary(admin)
        new = f"renamed-{secrets.token_hex(3)}.example"
        new_id = bytes(add_local_domain(admin, new)["domain"]["domain_id"])

        rename = _start(admin, new_id)
        assert rename["state"] == "cert_issuance", rename

        for domain in (old, new):
            with pytest.raises(RpcCallError) as refused:
                admin.call("fauna.bridges.remove_local_domain", {"domain": domain})
            assert refused.value.code == "fauna.bridges.domain_in_rename_flight", (
                domain, refused.value)
        assert {old, new} <= {d["domain_name"] for d in active_domains(admin)}

        aborted = admin.call("fauna.bridges.abort_primary_domain_rename", {
            "rename_id": bytes(rename["rename_id"]),
        })["rename"]
        assert aborted["state"] == "aborted", aborted

        admin.call("fauna.bridges.remove_local_domain", {"domain": new})
        assert new not in {d["domain_name"] for d in active_domains(admin)}


def _advance_mta_sts(nest, shift_days: int) -> list[str]:
    """Run the MTA-STS advance's clock half once at ``now + shift_days``
    through the ``test-hooks`` endpoint — the production statement the hourly
    pass runs, minus the trust half a harness nest can never satisfy
    (``mail-multidomain.md`` § The advance). Returns the names it moved."""
    import json
    import urllib.request

    from helpers.budgets import RPC_ROUNDTRIP_S

    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/mail-domains/mta-sts/advance",
        data=json.dumps({"shift_days": shift_days}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=RPC_ROUNDTRIP_S) as resp:
        assert resp.status == 200, f"mta-sts advance hook returned {resp.status}"
        return json.loads(resp.read())["advanced"]


def _modes(admin) -> dict[str, str]:
    return {d["domain_name"]: d["mta_sts_mode"] for d in active_domains(admin)}


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_rename_onto_a_weaker_tls_posture_is_refused_until_it_catches_up(rename_nest):
    """Outcome 50: a rename onto a domain whose MTA-STS posture is weaker than
    the current primary's is refused ``new_primary_tls_posture_weaker`` and
    starts nothing; once the target's own testing window runs out and it
    advances to ``enforce``, the same start is accepted — the remedy is to
    wait (``mail-primary-domain-rename.md`` § Don't do these)."""
    with _admin(rename_nest) as admin:
        set_host_address(admin)
        old = ensure_primary(admin)
        assert _modes(admin)[old] == "testing"

        assert old in _advance_mta_sts(rename_nest, 8)
        assert _modes(admin)[old] == "enforce"

        new = f"renamed-{secrets.token_hex(3)}.example"
        new_id = bytes(add_local_domain(admin, new)["domain"]["domain_id"])
        assert _modes(admin)[new] == "testing"

        with pytest.raises(RpcCallError) as refused:
            _start(admin, new_id)
        assert refused.value.code == "fauna.bridges.new_primary_tls_posture_weaker", (
            refused.value)
        status = admin.call("fauna.bridges.get_primary_domain_rename_status", {})
        assert status["rename"] is None, status

        assert new in _advance_mta_sts(rename_nest, 8)
        modes = _modes(admin)
        assert (modes[old], modes[new]) == ("enforce", "enforce"), modes

        rename = _start(admin, new_id)
        assert rename["state"] == "cert_issuance", rename

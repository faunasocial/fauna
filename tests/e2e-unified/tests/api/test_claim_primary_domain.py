"""tier_3 API e2e: admin claim registers the handle's domain as the primary
``mail_domains`` row (which IS the deployment identity), so the Admin→DNS
record matrix is non-empty.

Regression proof for the "claimed with an email handle but Admin→DNS shows no
records" bug. A real (non-local) ``mail_domain`` carried by
``fauna.auth.claim_admin`` becomes the primary ``mail_domains`` row
(``bins/fauna-nest/src/claim_core.rs`` → ``mail_enable::ensure_mail_domain_registered``
→ ``identity_domain_core::apply_primary_identity``), so:

* the claim reply's ``domain`` follows the handle (``state.handle_domain()``);
* ``fauna.bridges.list_local_domains`` lists it as ``is_primary``;
* ``fauna.dns.list_records`` returns a non-empty matrix — the exact record set
  the Admin→DNS page renders (empty was the bug).

A LOCAL target (IP literal / ``localhost`` / ``*.localhost`` / ``.local``)
registers NO mail domain — ``mail.<ip>`` is nonsense — and the identity stays
the ``"localhost"`` fallback (the ``claim_core.rs`` ``is_local`` gate at
``!resolve_handle_domain(d).is_local``), so Admin→DNS is correctly empty.

These tests each start their OWN unclaimed nest via ``start_nest(..., unclaimed=True)``
and drive the claim themselves (the session ``nest_instance`` is pre-claimed
and pre-seeds a primary domain, which would mask this behavior). The
``common.claim_admin`` helper accepts ``mail_domain`` but discards
``reply["domain"]`` — the very field this regression asserts — so the claim is
driven manually here to read the full reply.

Wire contracts verified 2026-07-02:

* ``ClaimAdminReply.domain`` — ``libs/fauna-protocol/src/claim.rs:80``.
* ``ListLocalDomainsReply {active, soft_deleted_within_30d}`` of ``MailDomainRow
  {domain_name, is_primary, ...}`` — ``libs/fauna-protocol/src/bridge_routing.rs:1146,1043``.
* ``ListRecordsReply {domains: [DomainDns {domain, mode, is_primary, records:
  [DnsRecordView {name, record_type, expected, ttl_seconds}]}]}`` —
  ``libs/fauna-protocol/src/dns.rs:77``; record-type labels
  ``MX|TXT|SRV|A|AAAA`` at ``bins/fauna-nest/src/dns_handlers.rs:89``.
"""

import time

from common import CLAIM_CODE
from common.auth import port_base_url

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def claim_target_nest(request, nest_mode, tmp_path_factory):
    """An UNCLAIMED nest, one per test: the claim itself is the subject here, so
    the harness must not have performed it.

    Function-scoped deliberately — the two tests claim the same nest shape onto
    a real domain and onto a local target, and a claim code is single-use.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "claim-target-nest", unclaimed=True)
    yield nest
    cleanup()


def _claim(port, *, handle, mail_domain):
    """Drive ``fauna.auth.claim_admin`` manually so we can read the FULL reply.

    Mirrors ``common.auth.claim_admin`` (fresh Ed25519 keypair; signature over
    ``verify_key ‖ timestamp_be_u64``), but returns the whole reply dict —
    ``common.auth.claim_admin`` drops ``reply["domain"]``, which is exactly the
    field this regression asserts. Returns ``(reply_dict, signing_key)``; the
    ``signing_key`` re-opens as the claimed admin for the follow-up admin calls.
    """
    from nacl.signing import SigningKey

    sk = SigningKey.generate()
    actor_id_hex = bytes(sk.verify_key).hex()
    timestamp = int(time.time())  # seconds; signature is over the raw value
    from common.sig_domain import claim_admin_signed_message

    msg = claim_admin_signed_message(bytes(sk.verify_key), timestamp)
    sig = sk.sign(msg).signature

    with WsRpcAnonClient(port_base_url(port)) as anon:
        reply = anon.call(
            "fauna.auth.claim_admin",
            {
                "claim_code": CLAIM_CODE,
                "actor_id": actor_id_hex,
                "signature": sig.hex(),
                "timestamp": timestamp,
                "handle": handle,
                "mail_domain": mail_domain,
            },
        )
    return reply, sk


def _admin_client(port, sk):
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=bytes(sk.verify_key),
        signing_key=bytes(sk),
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_claim_real_domain_registers_primary_and_dns(claim_target_nest):
    """A real ``mail_domain`` at claim → primary domain + non-empty Admin→DNS."""
    port = claim_target_nest["port"]
    reply, sk = _claim(port, handle="admin", mail_domain="example.com")

    # (1) The identity followed the claimed handle's domain — the claim reply
    # reports `state.handle_domain()`, now the just-registered primary.
    assert reply["domain"] == "example.com", (
        f"claim reply domain should follow the handle's mail_domain; got {reply!r}"
    )

    client = _admin_client(port, sk)
    with client:
        # (2) The domain is the SOLE primary `mail_domains` row.
        listed = client.call("fauna.bridges.list_local_domains", {})
        active_names = [d["domain_name"] for d in listed["active"]]
        assert active_names == ["example.com"], (
            f"expected exactly [example.com] active, got {active_names!r}"
        )
        assert listed["active"][0]["is_primary"] is True, listed["active"][0]

        # (3) The Admin→DNS record matrix is NON-EMPTY (empty was the bug).
        dns = client.call("fauna.dns.list_records", {})
        domains = dns["domains"]
        assert domains, f"Admin→DNS matrix must be non-empty; got {dns!r}"
        matrix = next(d for d in domains if d["domain"] == "example.com")
        assert matrix["records"], (
            f"no DNS records for the primary domain: {matrix!r}"
        )
        types = {r["record_type"] for r in matrix["records"]}
        # The record set the Admin→DNS page renders includes at minimum the
        # MX row and the SPF/DMARC TXT rows (dns_handlers.rs record labels).
        assert "MX" in types, (
            f"Admin→DNS record set must include an MX row; got {sorted(types)!r}"
        )
        assert "TXT" in types, (
            f"Admin→DNS record set must include SPF/DMARC TXT rows; "
            f"got {sorted(types)!r}"
        )

def test_claim_local_target_registers_no_mail_domain(claim_target_nest):
    """A LOCAL target at claim → no mail domain, localhost identity, empty DNS."""
    port = claim_target_nest["port"]
    reply, sk = _claim(port, handle="admin", mail_domain="127.0.0.1")

    # (1) A local target (IP literal) is an access address, not a mail domain:
    # nothing is registered and the identity stays the `localhost` fallback.
    assert reply["domain"] != "127.0.0.1", (
        f"a local IP must NOT become the identity domain; got {reply!r}"
    )
    assert reply["domain"] == "localhost", (
        f"local claim identity should fall back to localhost; got {reply!r}"
    )

    client = _admin_client(port, sk)
    with client:
        # (2) No mail domain registered.
        listed = client.call("fauna.bridges.list_local_domains", {})
        assert listed["active"] == [], (
            f"a local claim must register no mail domain; got {listed!r}"
        )

        # (3) The Admin→DNS matrix is empty — no primary domain exists.
        dns = client.call("fauna.dns.list_records", {})
        assert dns["domains"] == [], (
            f"Admin→DNS must be empty with no primary domain; got {dns!r}"
        )

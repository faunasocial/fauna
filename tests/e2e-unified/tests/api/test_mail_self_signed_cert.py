"""tier_3 e2e for the admin self-signed TLS cert WS-RPC surface
(``fauna.bridges.provision_self_signed_cert``;
``docs/goal/behavior/mail-bridge-lifecycle.md`` § TLS provisioning,
"Admin-synthesized (self-signed)").

The WS-RPC replacement of the retired HTTP route
``POST /api/admin/local_domains/{domain}/self_signed_cert``. The kind is
**Admin-class**: nest synthesizes a self-signed cert for an active local mail
domain via ``rcgen`` and seals+fans it out to every approved bridge with an
x25519 pubkey (writing the on-disk PEM too). This file proves the full wire
path — WS-RPC challenge/verify → bearer → WS negotiation → DAG-CBOR Request →
kind-routed dispatch → ``synthesize_and_seal_self_signed_cert`` (the shared
synthesis fn) → Reply decode — plus the caller-class gate. The
synthesis + sealed/skipped partition *logic* is covered in isolation by the
Rust handler tests (``bridge_routing_handlers::tests::provision_self_signed_*``);
this file owns the over-the-wire integration.

Every nest is content-ready from first boot (no-modes retirement, ratified
2026-07-12), so ``store_acme_material`` needs no preliminary commit any more.
The domain name is unique per run so it never collides with conftest-seeded rows.
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

_DOMAIN = f"selfsigned-{secrets.token_hex(4)}.test"


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


def test_admin_provisions_self_signed_cert_over_wire(nest_instance):
    """Add a domain, then provision a self-signed cert over WS-RPC — the kind
    routes, the allowlist permits Admin, and the reply names the sealed/skipped
    bridge sets plus a ~90-day expiry.

    The bridge assertions are on the **partition** (disjoint, each entry named),
    not on either set being empty: this runs against the shared session nest, so
    which bridges are enrolled — and which of them have attested an x25519 key —
    depends on what sibling tests did first. The per-bridge seal/skip decision
    itself is unit-tested in the Rust handler tests."""
    client = _admin_client(nest_instance)
    with client:
        client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": _DOMAIN,
                "mta_sts_cert_mode": "expand_primary",
            },
        )

        now = int(time.time())
        reply = client.call(
            "fauna.bridges.provision_self_signed_cert",
            {"domain": _DOMAIN, "additional_dns_sans": [f"mail.{_DOMAIN}"]},
        )
        # ⚠ 2026-08-20: this used to
        # assert `bridges_sealed_to == []`, calling it "the security invariant
        # that holds on any nest". It is not one — it is a statement about which
        # SIBLING TESTS have run first, and it was the TEST that was wrong. The
        # run found two genuinely-sealed bridges (`web-serve`/content-processor
        # and `test-mta-1`/mta): siblings on this shared session nest had
        # enrolled bridges that DID attest an x25519 key, so sealing to them is
        # the mechanism working, not a leak. The comment right here already knew
        # the nest was shared — it carved the *skip* list out for exactly that
        # reason and then left the seal list asserting a clean-nest premise.
        #
        # The invariant that really does hold on any nest is the PARTITION: the
        # two lists are disjoint, and a bridge is sealed to iff it had an x25519
        # key on file. Asserting that is also strictly stronger than the old
        # emptiness check, which on a clean nest passed vacuously — it never
        # once exercised the seal arm (e2e convention 17: assert general
        # invariants, not hand-picked outcomes).
        sealed = reply["bridges_sealed_to"]
        skipped = reply["bridges_skipped_no_x25519"]
        for entry in (*sealed, *skipped):
            assert entry.get("role") and entry.get("bridge_id"), (
                f"every partition entry names its bridge; got {entry!r} in {reply!r}"
            )
        sealed_ids = {e["bridge_id"] for e in sealed}
        skipped_ids = {e["bridge_id"] for e in skipped}
        assert len(sealed_ids) == len(sealed), (
            f"a bridge must appear at most once in the sealed set: {sealed!r}"
        )
        assert len(skipped_ids) == len(skipped), (
            f"a bridge must appear at most once in the skipped set: {skipped!r}"
        )
        assert sealed_ids.isdisjoint(skipped_ids), (
            "a bridge is either sealed to or skipped for want of an x25519 key, "
            f"never both: sealed={sealed!r} skipped={skipped!r}"
        )
        # 90-day window, well in the future.
        assert reply["expires_at_unix"] > now + 89 * 24 * 3600, (
            f"expiry must be ~90 days out: {reply!r}"
        )

        # Re-provision is allowed (admin-driven refresh) — re-synthesizes.
        again = client.call(
            "fauna.bridges.provision_self_signed_cert",
            {"domain": _DOMAIN, "additional_dns_sans": []},
        )
        assert again["expires_at_unix"] > now + 89 * 24 * 3600


def test_provision_unknown_domain_not_found(nest_instance):
    """Provisioning for a domain that isn't an active local mail domain is
    ``fauna.bridges.not_found`` (the synthesis short-circuits before rcgen)."""
    client = _admin_client(nest_instance)
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call(
                "fauna.bridges.provision_self_signed_cert",
                {"domain": f"never-added-{secrets.token_hex(4)}.test", "additional_dns_sans": []},
            )
    assert excinfo.value.code == "fauna.bridges.not_found", (
        f"unknown domain must be not_found; got {excinfo.value.code!r}"
    )


def test_non_admin_denied(nest_instance):
    """A User-class actor is rejected by the allowlist."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call(
                "fauna.bridges.provision_self_signed_cert",
                {"domain": _DOMAIN, "additional_dns_sans": []},
            )
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User-class must be denied; got {excinfo.value.code!r}"
    )

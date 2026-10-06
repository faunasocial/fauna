"""tier_3 e2e for the local-domain admin WS-RPC surface
(``fauna.bridges.{add,remove,restore,list,update}_local_domain``).

The mission these kinds serve: the
mail bridge is configurable entirely from a Fauna app, over
WS-RPC, with no HTTP. This file proves the full wire path for an Admin
caller — HTTP challenge/verify → bearer → WS subprotocol negotiation →
canonical DAG-CBOR Request encode → kind-routed dispatch → handler →
``mail_domains`` table → Reply decode — plus the caller-class gate. The
add/remove/restore/primary-guard *business logic* is covered in isolation
by the Rust handler tests (``bridge_routing_handlers::tests``); this file
only owns the over-the-wire integration.

``nest_instance`` is session-scoped (shared, and the conftest seeds some
domains via the legacy HTTP route), so these tests use a unique domain
name and never assume an empty ``mail_domains`` table or assert absolute
``is_primary``.
"""

import secrets

import pytest

pytestmark = [pytest.mark.tier_3]

# Unique per test-run so we never collide with conftest-seeded domains or
# a previous run sharing the session-scoped nest.
_SUFFIX = secrets.token_hex(4)
_DOMAIN = f"wsrpc-{_SUFFIX}.test"


def _admin_client(nest_instance):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_adds_and_lists_local_domain_over_wire(nest_instance):
    """Add a domain over WS-RPC, see it in the list, re-add is a no-op.

    Proves the whole admin control-plane wire path end-to-end: the kind is
    registered, the allowlist permits Admin, the DAG-CBOR Request/Reply
    round-trips over the real socket, and the row lands in ``mail_domains``.
    """
    client = _admin_client(nest_instance)
    with client:
        reply = client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": _DOMAIN,
                "mta_sts_cert_mode": "expand_primary",
            },
        )
        assert reply["skipped"] is False, f"first add must not be skipped: {reply!r}"
        assert reply["domain"]["domain_name"] == _DOMAIN
        # No request names an MTA-STS mode: the nest stores every new domain
        # `testing` and advances it by itself (mail-multidomain.md § The advance).
        assert reply["domain"]["mta_sts_mode"] == "testing"
        # dkim_algorithms is the parsed list, not the JSON-text column.
        assert reply["domain"]["dkim_algorithms"] == ["ed25519", "rsa-2048"]

        listed = client.call("fauna.bridges.list_local_domains", {})
        names = [d["domain_name"] for d in listed["active"]]
        assert _DOMAIN in names, f"added domain absent from list: {names!r}"

        # Idempotent on domain_name (mail-multidomain.md § add :349).
        again = client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": _DOMAIN,
                "mta_sts_cert_mode": "expand_primary",
            },
        )
        assert again["skipped"] is True, f"re-add must be skipped: {again!r}"

        # Update an always-present per-domain knob; confirm it round-trips.
        updated = client.call(
            "fauna.bridges.update_local_domain_config",
            {"domain": _DOMAIN, "mta_sts_max_age_seconds": 604800},
        )
        assert updated["domain"]["mta_sts_max_age_seconds"] == 604800
        # The mode is not an update field either: it is still the nest's `testing`.
        assert updated["domain"]["mta_sts_mode"] == "testing"

        # Clean up after ourselves in the shared session nest: soft-delete
        # the domain unless it became the deployment primary (empty-table
        # case — the primary can't be removed, which is itself correct).
        if not reply["domain"]["is_primary"]:
            removed = client.call(
                "fauna.bridges.remove_local_domain", {"domain": _DOMAIN}
            )
            assert removed["domain"]["removed_at"] is not None


@pytest.mark.feature("admin-dns-and-certificates")
def test_non_admin_denied_on_local_domain_kind(nest_instance, test_user):
    """A User-class actor is rejected by the allowlist before any DB work."""
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call(
                "fauna.bridges.add_local_domain",
                {
                    "domain": f"denied-{_SUFFIX}.test",
                    "mta_sts_cert_mode": "expand_primary",
                },
            )
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User-class actor on an Admin kind must be denied; "
        f"got {excinfo.value.code!r}"
    )


def _dmarc_policy_tags(client, domain):
    """The ``p=`` and ``sp=`` of ``domain``'s published ``_dmarc`` record."""
    domains = client.call("fauna.dns.list_records", {"domain": domain})["domains"]
    records = [
        r
        for d in domains
        for r in d["records"]
        if r["record_type"] == "TXT" and r["name"].rstrip(".") == f"_dmarc.{domain}"
    ]
    assert len(records) == 1, f"expected one _dmarc record for {domain}: {domains!r}"
    tags = dict(
        part.strip().split("=", 1)
        for part in records[0]["expected"].split(";")
        if "=" in part
    )
    return tags["p"], tags["sp"]


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_softens_one_domains_dmarc_policy_over_wire(nest_instance, test_user):
    """An admin softens one domain's published DMARC policy, and reverts it.

    ``dmarc-reporting.md`` § Multi-domain deployments: the write sets the
    domain's ``p=`` and ``sp=`` together, leaves every other domain's record
    alone, and choosing the default (``reject``) restores the deployment
    policy. A User-class actor cannot make the write.
    """
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    soft = f"dmarc-soft-{_SUFFIX}.test"
    strict = f"dmarc-strict-{_SUFFIX}.test"
    client = _admin_client(nest_instance)
    with client:
        added = {}
        for d in (soft, strict):
            added[d] = client.call(
                "fauna.bridges.add_local_domain",
                {"domain": d, "mta_sts_cert_mode": "expand_primary"},
            )["domain"]
        assert _dmarc_policy_tags(client, soft) == ("reject", "reject")

        updated = client.call(
            "fauna.bridges.update_local_domain_config",
            {"domain": soft, "dmarc_policy_mode": "quarantine"},
        )
        assert updated["domain"]["dmarc_overrides"]["policy_mode"] == "quarantine"
        assert _dmarc_policy_tags(client, soft) == ("quarantine", "quarantine")
        assert _dmarc_policy_tags(client, strict) == ("reject", "reject")

        restored = client.call(
            "fauna.bridges.update_local_domain_config",
            {"domain": soft, "dmarc_policy_mode": "reject"},
        )
        # The default clears the override rather than storing it.
        assert "policy_mode" not in restored["domain"]["dmarc_overrides"]
        assert _dmarc_policy_tags(client, soft) == ("reject", "reject")

        user = WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )
        with user:
            with pytest.raises(RpcCallError) as excinfo:
                user.call(
                    "fauna.bridges.update_local_domain_config",
                    {"domain": soft, "dmarc_policy_mode": "none"},
                )
        assert excinfo.value.code == "fauna.bridges.permission_denied"
        assert _dmarc_policy_tags(client, soft) == ("reject", "reject")

        for d, row in added.items():
            if not row["is_primary"]:
                client.call("fauna.bridges.remove_local_domain", {"domain": d})

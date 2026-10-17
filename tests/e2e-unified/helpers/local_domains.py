"""Admin-side helpers for a nest's hosted mail domains, over WS-RPC.

Shared by the api-tier domain-lifecycle and primary-domain-rename witnesses
and the docker-tier rename witness. Each takes a connected admin client
(``WsRpcAdminClient`` or the docker helpers' ``admin_ws``), never a nest dict,
so one helper serves every tier.

Wire contracts: ``fauna.bridges.add_local_domain`` / ``list_local_domains`` —
``libs/fauna-protocol/src/bridge_routing.rs`` (``AddLocalDomainRequest``,
``ListLocalDomainsReply {active, soft_deleted_within_30d}``);
``fauna.dns.set_host_address`` — ``libs/fauna-protocol/src/dns.rs``.
"""

from __future__ import annotations

import secrets

# Documentation-range addresses (RFC 5737): two different machines.
DOC_NEST_IP = "203.0.113.10"
DOC_MAIL_IP = "198.51.100.20"


def add_local_domain(admin, domain: str) -> dict:
    """Add ``domain`` anchoring the shared certificate (``expand_primary``) —
    the cert mode a rename target must carry."""
    return admin.call("fauna.bridges.add_local_domain", {
        "domain": domain,
        "mta_sts_cert_mode": "expand_primary",
    })


def active_domains(admin) -> list[dict]:
    return admin.call("fauna.bridges.list_local_domains", {})["active"]


def primary_domain(admin) -> str | None:
    primary = [d["domain_name"] for d in active_domains(admin) if d["is_primary"]]
    return primary[0] if primary else None


def ensure_primary(admin) -> str:
    """The nest's primary domain, adding one when the nest has none — so a
    domain a test removes or renames onto is never the primary."""
    primary = primary_domain(admin)
    if primary:
        return primary
    name = f"primary-{secrets.token_hex(3)}.example"
    assert add_local_domain(admin, name)["domain"]["is_primary"] is True
    return name


def set_host_address(admin, nest_ipv4: str = DOC_NEST_IP, mail_ipv4: str = DOC_MAIL_IP) -> None:
    """Persist the deployment's host addresses — what the DNS assembler
    publishes ``mail.<domain>`` at, and what a rename's ``mail.<new>``
    certificate gate checks against (strong-check-or-DROP without it)."""
    admin.call("fauna.dns.set_host_address", {
        "nest_ipv4": nest_ipv4,
        "nest_ipv6": None,
        "mail_ipv4": mail_ipv4,
        "mail_ipv6": None,
    })

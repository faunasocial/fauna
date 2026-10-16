"""tier_3 API e2e: the nest's half of a hosted domain's lifecycle on the
Domains page — what a removed domain refuses, what a restore keeps, and the
DMARC record each domain publishes.

Each test runs on its OWN nest (``_start_dedicated_nest``): removing and
restoring domains is a state change the session-shared ``nest_instance`` must
never see.

Wire contracts:

* ``fauna.bridges.add_local_domain`` / ``remove_local_domain`` /
  ``restore_local_domain`` / ``list_local_domains`` —
  ``libs/fauna-protocol/src/bridge_routing.rs`` (``AddLocalDomainRequest``,
  ``ListLocalDomainsReply {active, soft_deleted_within_30d}``).
* ``fauna.dns.list_records`` → ``{domains: [{domain, records: [{name,
  record_type, expected, ...}]}]}`` — ``libs/fauna-protocol/src/dns.rs``.
* The ``domain_pending_recovery`` refusal —
  ``bins/fauna-nest/src/bridge_routing_handlers.rs::domain_pending_recovery``.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from helpers.local_domains import (
    DOC_MAIL_IP as _MAIL_IP,
    DOC_NEST_IP as _NEST_IP,
    add_local_domain as _add,
    ensure_primary as _ensure_primary,
    set_host_address as _set_host_address,
)

pytestmark = pytest.mark.tier_3


@pytest.fixture
def domain_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "domain-lifecycle-nest")
    try:
        yield nest
    finally:
        cleanup()


def _admin(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _records(admin, domain: str) -> list[dict]:
    matrix = admin.call("fauna.dns.list_records", {})["domains"]
    rows = [d for d in matrix if d["domain"] == domain]
    assert rows, f"{domain} has no record matrix: {[d['domain'] for d in matrix]!r}"
    return rows[0]["records"]


def _remove(admin, domain: str) -> None:
    admin.call("fauna.bridges.remove_local_domain", {"domain": domain})


def _restore(admin, domain: str) -> None:
    admin.call("fauna.bridges.restore_local_domain", {"domain": domain})


@pytest.mark.feature("admin-dns-and-certificates")
def test_adding_a_recently_removed_domain_is_refused_toward_restore(domain_nest):
    """Outcome 30: an Add of a domain removed within the recovery window is
    refused ``domain_pending_recovery``, creates no second row, and the Restore
    it points at still brings the domain back."""
    with _admin(domain_nest) as admin:
        _ensure_primary(admin)
        domain = f"gone-{secrets.token_hex(3)}.example"
        _add(admin, domain)
        _remove(admin, domain)

        with pytest.raises(RpcCallError) as refused:
            _add(admin, domain)
        assert refused.value.code == "fauna.bridges.domain_pending_recovery", refused.value

        listed = admin.call("fauna.bridges.list_local_domains", {})
        assert domain not in [d["domain_name"] for d in listed["active"]], listed
        assert [d["domain_name"] for d in listed["soft_deleted_within_30d"]] == [domain], listed

        _restore(admin, domain)
        listed = admin.call("fauna.bridges.list_local_domains", {})
        assert domain in [d["domain_name"] for d in listed["active"]], listed
        assert listed["soft_deleted_within_30d"] == [], listed


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_restored_domain_publishes_the_same_signing_key(domain_nest):
    """Outcome 34: the DKIM record a restored domain asks DNS to carry is the
    one it published before its removal — the key was kept, not re-minted."""
    with _admin(domain_nest) as admin:
        _ensure_primary(admin)
        domain = f"back-{secrets.token_hex(3)}.example"
        _add(admin, domain)

        def dkim() -> dict[str, str]:
            return {
                r["name"]: r["expected"]
                for r in _records(admin, domain)
                if r["name"].endswith(f"._domainkey.{domain}")
            }

        before = dkim()
        assert before, f"{domain} lists no DKIM record: {_records(admin, domain)!r}"
        assert all("p=" in v for v in before.values()), before

        _remove(admin, domain)
        _restore(admin, domain)
        assert dkim() == before


@pytest.mark.feature("admin-dns-and-certificates")
def test_each_domain_publishes_a_strict_reject_dmarc_policy_reporting_to_the_nest(domain_nest):
    """Outcome 27: every hosted domain's DMARC record asks receivers to reject,
    for the domain and its subdomains, at one hundred per cent, with strict
    alignment, and sends aggregate reports to the nest's primary domain."""
    with _admin(domain_nest) as admin:
        primary = _ensure_primary(admin)
        second = f"second-{secrets.token_hex(3)}.example"
        _add(admin, second)

        for domain in (primary, second):
            dmarc = [r for r in _records(admin, domain) if r["name"] == f"_dmarc.{domain}"]
            assert len(dmarc) == 1, _records(admin, domain)
            assert dmarc[0]["record_type"] == "TXT", dmarc
            tags = dict(
                part.strip().split("=", 1)
                for part in dmarc[0]["expected"].strip('"').split(";")
                if part.strip()
            )
            assert tags["v"] == "DMARC1", tags
            assert tags["p"] == "reject", tags
            assert tags["sp"] == "reject", tags
            assert tags["pct"] == "100", tags
            assert tags["adkim"] == "s", tags
            assert tags["aspf"] == "s", tags
            assert tags["rua"].startswith("mailto:"), tags
            assert tags["rua"].endswith(f"@{primary}"), tags


def _run_domain_gc(nest, shift_days: int) -> list[str]:
    """Run the nest's removed-domain GC once at ``now + shift_days`` through the
    ``test-hooks`` endpoint — the production sweep the background-expiry tick
    runs, at a clock this tier cannot wait out. Returns the names it removed."""
    import json
    import urllib.request

    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/mail-domains/gc/run",
        data=json.dumps({"shift_days": shift_days}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    from helpers.budgets import RPC_ROUNDTRIP_S

    with urllib.request.urlopen(req, timeout=RPC_ROUNDTRIP_S) as resp:
        assert resp.status == 200, f"mail-domain gc hook returned {resp.status}"
        return json.loads(resp.read())["gone"]


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_removed_domain_is_restorable_for_thirty_days_then_gone(domain_nest):
    """Outcome 35: a removed domain survives the GC inside its thirty-day window;
    past it the domain is gone from both lists, a restore is refused, a member's
    address on it is gone, and adding the name again mints a NEW signing key."""
    from conftest import _make_user
    from helpers.mail_aliases import add_exact_alias

    with _admin(domain_nest) as admin:
        _ensure_primary(admin)
        domain = f"expire-{secrets.token_hex(3)}.example"
        _add(admin, domain)

        def dkim() -> dict[str, str]:
            return {
                r["name"]: r["expected"]
                for r in _records(admin, domain)
                if r["name"].endswith(f"._domainkey.{domain}")
            }

        before = dkim()
        assert before, f"{domain} lists no DKIM record: {_records(admin, domain)!r}"

        member = _make_user(domain_nest)
        local = f"m{secrets.token_hex(3)}"
        add_exact_alias(domain_nest["url"], member["signing_key"], domain, local)

        def member_holds_address() -> bool:
            with WsRpcAdminClient(
                domain_nest["url"],
                actor_id=bytes(member["signing_key"].verify_key),
                signing_key=bytes(member["signing_key"]),
            ) as ws:
                rows = ws.call("fauna.bridges.list_account_aliases", {})["aliases"]
            return any(
                r["local_domain"].lower() == domain and r["pattern"].lower() == local
                for r in rows
            )

        assert member_holds_address()
        _remove(admin, domain)

        # Inside the window: the sweep leaves it restorable.
        assert domain not in _run_domain_gc(domain_nest, 29)
        listed = admin.call("fauna.bridges.list_local_domains", {})
        assert [d["domain_name"] for d in listed["soft_deleted_within_30d"]] == [domain], listed
        assert member_holds_address(), "a dormant address must survive inside the window"

        # Past the window: gone for good.
        assert domain in _run_domain_gc(domain_nest, 31)
        listed = admin.call("fauna.bridges.list_local_domains", {})
        names = [d["domain_name"] for d in listed["active"] + listed["soft_deleted_within_30d"]]
        assert domain not in names, listed
        with pytest.raises(RpcCallError):
            _restore(admin, domain)
        assert not member_holds_address(), "the member's address must go with the domain"

        # The name is free again, and its signing key is a new one.
        _add(admin, domain)
        after = dkim()
        assert after, f"re-added {domain} lists no DKIM record: {_records(admin, domain)!r}"
        assert set(after.values()).isdisjoint(before.values()), (before, after)


def _a_records(admin, domain: str) -> dict[str, str]:
    return {
        r["name"]: r["expected"]
        for r in _records(admin, domain)
        if r["record_type"] == "A"
    }


@pytest.mark.feature("admin-dns-and-certificates")
def test_record_list_carries_the_nest_and_mail_host_addresses_separately(domain_nest):
    """Outcome 24: the primary's record list carries an address record for the
    nest's main name and one for its mail server, each pointing where the admin
    said — two different machines here."""
    with _admin(domain_nest) as admin:
        primary = _ensure_primary(admin)
        _set_host_address(admin)
        a = _a_records(admin, primary)
        assert a.get(primary) == _NEST_IP, a
        assert a.get(f"mail.{primary}") == _MAIL_IP, a


@pytest.mark.feature("admin-dns-and-certificates")
def test_identity_record_listed_on_every_public_domain_and_on_no_other(domain_nest):
    """Outcome 25: every public hosted domain lists the ``_fauna`` identity
    record carrying the same nest identity; a non-public domain lists none."""
    with _admin(domain_nest) as admin:
        primary = _ensure_primary(admin)
        second = f"second-{secrets.token_hex(3)}.example"
        private = f"lan-{secrets.token_hex(3)}.local"
        _add(admin, second)
        _add(admin, private)

        def identity(domain: str) -> list[dict]:
            return [r for r in _records(admin, domain) if r["name"] == f"_fauna.{domain}"]

        values = set()
        for domain in (primary, second):
            rows = identity(domain)
            assert len(rows) == 1, _records(admin, domain)
            assert rows[0]["record_type"] == "TXT", rows
            value = rows[0]["expected"].strip('"')
            assert value.startswith("self="), value
            assert len(bytes.fromhex(value.removeprefix("self="))) == 32, value
            values.add(value)
        assert len(values) == 1, f"one deployment identity on every domain: {values!r}"

        matrix = admin.call("fauna.dns.list_records", {})["domains"]
        private_rows = [d for d in matrix if d["domain"] == private]
        for d in private_rows:
            assert not [r for r in d["records"] if r["name"].startswith("_fauna.")], d


@pytest.mark.feature("admin-dns-and-certificates")
def test_approving_the_bluesky_server_adds_its_address_record(domain_nest):
    """Outcome 26, the Bluesky-server half: approving an ``atproto.pds`` bridge
    adds ``pds.<primary>``'s address record, pointing at the nest. (The relay
    half is gated on a connected relay sidecar, which this tier cannot run.)"""
    from nacl.signing import SigningKey

    from helpers.bridge_enrollment import enroll_and_approve_bridge

    with _admin(domain_nest) as admin:
        primary = _ensure_primary(admin)
        _set_host_address(admin)
        assert f"pds.{primary}" not in _a_records(admin, primary)

        bridge_key = SigningKey.generate()
        enroll_and_approve_bridge(
            domain_nest["url"], domain_nest["admin"]["signing_key"],
            bytes(bridge_key.verify_key), "atproto.pds",
        )
        assert _a_records(admin, primary).get(f"pds.{primary}") == _NEST_IP


def _nest_info(nest) -> dict:
    """The anonymous ``fauna.nest.info`` a device reads at sign-in."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.nest.info", {})


@pytest.mark.feature("admin-dns-and-certificates")
def test_the_first_domain_added_to_a_domainless_nest_becomes_its_name_at_once(domain_nest):
    """Outcome 58: on a nest that has no domain, the first one the admin adds
    becomes the nest's own name on the running nest — the handle domain and the
    web address a device reads from ``fauna.nest.info`` both follow it, with no
    restart in between (``domains-and-tls-bootstrap.md`` § Add domains after
    install)."""
    with _admin(domain_nest) as admin:
        active = admin.call("fauna.bridges.list_local_domains", {})["active"]
        assert not active, f"the premise: the nest must start with no domain; got {active!r}"
        before = _nest_info(domain_nest)
        first = f"first-{secrets.token_hex(3)}.example"
        assert first not in (before["domain"], before.get("web_serving_domain")), before

        assert _add(admin, first)["domain"]["is_primary"] is True

        after = _nest_info(domain_nest)
        assert after["nest_id"] == before["nest_id"], (before, after)
        assert after["domain"] == first, (
            f"the handle domain did not follow the first added domain: {after['domain']!r}")
        assert after.get("web_serving_domain") == first, (
            f"the web address did not follow the first added domain: "
            f"{after.get('web_serving_domain')!r}")

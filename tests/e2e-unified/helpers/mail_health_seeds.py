"""Put the session nest's mail health into a chosen state, from outside it.

Shared by the nest-half witnesses (``tests/api/test_mail_health.py``) and the
app-half journeys (``tests/test_admin_mail_health.py``): neither DNS nor the
minute is real. ``POST /api/v1/test/dns/record`` (the ``test-hooks`` DNS seam)
makes the outbound address ``mail.<primary>`` resolve and a DNSBL list it, or
makes a list not answer; ``POST /api/v1/test/outbound/clock`` moves the mail
clock past the once-a-minute force-refresh limit. Both hooks are present in
the standard e2e nest build.

Shared-nest hygiene: ``seeded_outbound`` ends by recording a real not-listed
answer for every list and removing its seeds, so the session nest's blocklist
state reads clean for the tests that follow.
"""

from __future__ import annotations

import contextlib
import json
import urllib.request

from clients.ws_rpc_admin_client import WsRpcAdminClient

#: The session nest's primary mail domain (`conftest.MAIL_PRIMARY_DOMAIN`); the
#: self-check's outbound address is the A record of `mail.<primary>`.
PRIMARY = "fauna.test"
#: A documentation address (RFC 5737) seeded as the outbound IP.
OUTBOUND_IP = "192.0.2.77"
#: The default self-check set (`DEFAULT_BLOCKLIST_SELF_CHECK_SERVERS`).
LISTS = ("zen.spamhaus.org", "b.barracudacentral.org", "bl.spamcop.net")
#: Past the per-list once-a-minute force-refresh limit.
PAST_THE_LIMIT = 61


def admin_client(nest) -> WsRpcAdminClient:
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def post(nest, path, body) -> None:
    req = urllib.request.Request(
        f"{nest['url']}{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"{path} returned {resp.status}"


def seed_dns_record(nest, name, record_type, values=(), transient=False) -> None:
    post(
        nest,
        "/api/v1/test/dns/record",
        {
            "name": name,
            "record_type": record_type,
            "values": list(values),
            "transient": transient,
        },
    )


def advance_mail_clock(nest, seconds) -> None:
    post(nest, "/api/v1/test/outbound/clock", {"advance": seconds})


def query_name(server) -> str:
    return ".".join(reversed(OUTBOUND_IP.split("."))) + "." + server


def answer_not_listed(nest, server) -> None:
    """The list answers, and the address is not on it (a non-127 A record)."""
    seed_dns_record(nest, query_name(server), "A", ["192.0.2.1"])


def answer_listed(nest, server, reason) -> None:
    seed_dns_record(nest, query_name(server), "A", ["127.0.0.2"])
    seed_dns_record(nest, query_name(server), "TXT", [reason])


def answer_nothing(nest, server) -> None:
    """The list does not answer at all (a timeout)."""
    seed_dns_record(nest, query_name(server), "A", transient=True)


def reverse_name(ip: str) -> str:
    return ".".join(reversed(ip.split("."))) + ".in-addr.arpa"


@contextlib.contextmanager
def passing_records(nest):
    """Publish, through the DNS seam, every record the diagnostics' SPF, DKIM,
    DMARC and reverse-DNS checks read, so the records row passes; removed again
    on the way out. The outbound address must already resolve
    (``seeded_outbound``)."""
    with admin_client(nest) as admin:
        selectors = admin.call("fauna.bridges.list_dkim_selectors", {"domain": PRIMARY})[
            "selectors"
        ]
    assert selectors, "the primary domain has no DKIM selector to publish"
    seeded: list[tuple[str, str]] = []

    def seed(name, record_type, values):
        seed_dns_record(nest, name, record_type, values)
        seeded.append((name, record_type))

    seed(PRIMARY, "TXT", ["v=spf1 mx ~all"])
    seed(f"_dmarc.{PRIMARY}", "TXT", ["v=DMARC1; p=reject"])
    for sel in selectors:
        seed(f"{sel['selector']}._domainkey.{PRIMARY}", "TXT", [sel["public_dns_value"]])
    seed(reverse_name(OUTBOUND_IP), "PTR", [f"mail.{PRIMARY}"])
    try:
        yield
    finally:
        for name, record_type in seeded:
            seed_dns_record(nest, name, record_type)


def run_self_check(nest) -> dict:
    """Move the clock past the force-refresh limit and run the blocklist
    self-check once; returns the run's reply."""
    advance_mail_clock(nest, PAST_THE_LIMIT)
    with admin_client(nest) as admin:
        return admin.call("fauna.bridges.blocklist_self_check_run", {})


@contextlib.contextmanager
def seeded_outbound(nest):
    """Make `mail.<primary>` resolve and every list answer "not listed"; on the
    way out record one real not-listed answer from every list and drop every
    seed, so the shared nest's standing result is clean again."""
    seed_dns_record(nest, f"mail.{PRIMARY}", "A", [OUTBOUND_IP])
    for server in LISTS:
        answer_not_listed(nest, server)
    try:
        yield nest
    finally:
        for server in LISTS:
            answer_not_listed(nest, server)
            seed_dns_record(nest, query_name(server), "TXT")
        run_self_check(nest)
        for server in LISTS:
            seed_dns_record(nest, query_name(server), "A")
        seed_dns_record(nest, f"mail.{PRIMARY}", "A")

"""tier_3 e2e: an admin whose app reaches the nest over its home network never
has that private address published as the nest's public address record.

`docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Host-address
acquisition: the admin's app reports the nest's public IP from the address it
dialled (`fauna.dns.set_host_address`, which makes the nest list the host
`A`/`PTR` rows) — and a private/LAN dial falls through to reporting NOTHING;
never a private address as `nest_ipv4`/`mail_ipv4`. The classification and the
report are shared Rust (`fauna_client_dns::host_address::report_host_address`,
`classify_dial_host`); each app fires it once per admin session.

The nest is `spki_pinned_nest`: a dedicated nest dialled on this box's own LAN
address (`common.nest.lan_ipv4()`), the real home-network case — and dedicated,
so no other test's `set_host_address` (the PTR-row test seeds a documentation
address on the shared nest) can stand in for this one's absence. A domain is
seeded first, because `fauna.dns.list_records` lists nothing for a nest with no
primary domain and the absence of host rows would then prove nothing.

Web reaches the nest through ``lan_spa_url``: an SPA proxy bound (over TLS, as a
LAN nest serves) on the same LAN address, so the browser's ``node_url`` — the
address web's report classifies — is the private one, while the proxy's own upstream hop stays on loopback (the nest
binds every interface when dialled on the LAN). The test asserts nothing about
certificate trust, so the proxy's trust posture is not under test here.

That the report RAN is read from the app's own log line for the skip (`info`,
once per admin session); that it published nothing is read twice — from the
records page through the app, and from the nest's own matrix (convention 5).
"""
from __future__ import annotations

import secrets

import pytest

from helpers.admin_wire import dns_matrix_for, seed_local_domain
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

#: `fauna_client_dns::host_address`'s record of a report that found no public
#: address to send.
SKIPPED_LINE = "host-address: no public IP determinable — reporting nothing"

#: The record types the nest emits only once a host address was reported.
HOST_RECORD_TYPES = {"A", "AAAA", "PTR"}


@pytest.fixture
def lan_spa_url(spa_proxy_for, spki_pinned_nest):
    """SPA proxy → ``spki_pinned_nest``, listening on this box's LAN address.

    The browser dials ``https://<lan>:<port>``, so web's dial-host classification
    sees the home-network case the native apps see on the nest's own URL. The
    upstream hop dials the nest's loopback authority on the same (TLS-marked)
    port, which the run's floor-cert posture accepts."""
    from urllib.parse import urlsplit

    from common.nest import lan_ipv4

    parts = urlsplit(spki_pinned_nest["url"])
    upstream = f"{parts.scheme}://127.0.0.1:{parts.port}"
    return spa_proxy_for(upstream, bind_host=lan_ipv4())


@pytest.fixture
def lan_admin_app(request, app, spki_pinned_nest):
    """``app`` signed in as the LAN-dialled nest's admin, on the admin shell."""
    from conftest import _login_admin_as

    _login_admin_as(
        app, request, spki_pinned_nest,
        spa_url_fixture="lan_spa_url", fixture_name="lan_admin_app",
    )
    return app


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.feature("admin-dns-and-certificates")
def test_an_admin_on_the_home_network_never_publishes_its_private_address(
    lan_admin_app, spki_pinned_nest
):
    from common.nest import lan_ipv4

    app = lan_admin_app
    nest = spki_pinned_nest
    lan = lan_ipv4()
    domain = f"home-{secrets.token_hex(4)}.test"
    seed_local_domain(nest, domain)

    wait_until(
        lambda: SKIPPED_LINE in app.driver.app_stderr_text() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the admin session never logged its host-address decision for a dial "
            f"of {nest['url']!r}; app stderr tail: "
            f"{app.driver.app_stderr_text()[-2000:]!r}"
        ),
    )

    app.admin.navigate_dns()
    want = sorted(
        (r["name"], r["record_type"], r["expected"]) for r in dns_matrix_for(nest, domain)
    )
    assert want, f"the nest lists no records for {domain!r}"

    def rendered():
        names = app.admin.dns_domain_names()
        if domain not in names:
            return None
        rows = app.admin.dns_records(names.index(domain))
        got = sorted((r["name"], r["type"], r["value"]) for r in rows)
        return rows if got == want else None

    wait_until(
        rendered,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"admin-dns never rendered {domain!r}'s records; sections "
            f"{app.admin.dns_domain_names()!r}, nest matrix {want!r}"
        ),
    )

    for i, section in enumerate(app.admin.dns_domain_names()):
        rows = app.admin.dns_records(i)
        host_rows = [r for r in rows if r["type"] in HOST_RECORD_TYPES]
        assert not host_rows, (
            f"the records page lists host address rows under {section!r} though the "
            f"admin only ever reached the nest at {lan}: {host_rows!r}"
        )
        leaked = [r for r in rows if lan in r["value"]]
        assert not leaked, (
            f"the private address {lan} is listed as a value to publish under "
            f"{section!r}: {leaked!r}"
        )
        nest_host_rows = [
            r for r in dns_matrix_for(nest, section)
            if r["record_type"] in HOST_RECORD_TYPES or lan in r["expected"]
        ]
        assert not nest_host_rows, (
            f"fauna.dns.list_records carries a host address for {section!r} after a "
            f"LAN-dialled admin session: {nest_host_rows!r}"
        )

"""tier_3 e2e: the admin unified-DNS page (`admin-dns`) renders, per domain, the
**served-TLS-cert health badge** (`admin-dns-cert-status`) next to the per-record
red/green DNS checks — `tls-certificates.md` § C.4.

The nest computes the state server-side from the leaf it actually serves
(`fauna.tls.cert_status`); the client renders `snapshot.cert_statuses[domain]` via
`DnsAction::RefreshCertStatus`. A fresh test nest has no CA-issued cert for a
synthetic `.test` domain, so it serves the **self-signed floor** → the badge
reads the on-floor / renew-needed state. This is a pure read, so it renders on
all apps (web included).

Mirrors `test_admin_dns.py`'s seed-then-poll idiom; `nest_instance` is
session-scoped so we seed a unique domain and assert *that* domain's badge.
"""

import secrets
import time

import pytest

from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

_DOMAIN = f"admincert-{secrets.token_hex(4)}.test"


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane (mirrors
    ``test_admin_dns.py::_seed_local_domain``)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": domain,
                "mta_sts_cert_mode": "expand_primary",
            },
        )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_cert_status_renders_on_floor(admin_app, nest_instance):
    """A seeded domain's `admin-dns-cert-status` badge renders the on-floor /
    renew-needed state — the fresh test nest serves the self-signed floor for the
    synthetic domain (no CA cert), which `fauna.tls.cert_status` reports as
    `on-floor — renew needed` (`tls-certificates.md` § C.4)."""
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_dns()

    on_floor = S.admin.dns.cert.status_on_floor
    self_signed = S.admin.dns.cert.self_signed

    deadline = time.time() + 15.0
    cert_texts: list[str] = []
    domain_names: list[str] = []
    while time.time() < deadline:
        domain_names = [
            admin_app.driver.get_text("admin-dns-domain-name", index=i) or ""
            for i in range(admin_app.driver.count("admin-dns-domain-name"))
        ]
        cert_texts = [
            admin_app.driver.get_text("admin-dns-cert-status", index=i) or ""
            for i in range(admin_app.driver.count("admin-dns-cert-status"))
        ]
        # One cert-status badge per domain section; the fresh nest serves the
        # floor for every domain, so any rendered badge shows the on-floor state.
        if _DOMAIN in " ".join(domain_names) and any(
            on_floor in t or self_signed in t for t in cert_texts
        ):
            break
        time.sleep(0.5)

    err = admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"
    assert _DOMAIN in " ".join(domain_names), (
        f"seeded domain {_DOMAIN!r} not found among {domain_names!r} (error: {err})"
    )
    # A cert-status badge renders per domain (one per admin-dns-domain section).
    assert len(cert_texts) >= len([d for d in domain_names if d]), (
        f"expected one admin-dns-cert-status per domain; domains={domain_names!r} "
        f"badges={cert_texts!r} (error: {err})"
    )
    # The served cert for a synthetic .test domain is the self-signed floor →
    # the badge reads the on-floor / renew-needed (self-signed) state.
    assert any(on_floor in t or self_signed in t for t in cert_texts), (
        f"no admin-dns-cert-status badge shows the on-floor state "
        f"({on_floor!r}/{self_signed!r}); got {cert_texts!r} (error: {err})"
    )

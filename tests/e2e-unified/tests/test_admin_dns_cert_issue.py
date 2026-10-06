"""tier_3 e2e: the admin `admin-dns` page renders, per domain, the TLS-cert
**issuance** affordance (`admin-dns-cert-issue-button`) — tls-certificates.md
§ B tier 2/3 (S5/S6a).

A managed or CNAME-delegated domain's button fires a single client-driven DNS-01
order (`DnsAction::IssueCert`); a manual (no covering credential, undelegated)
domain's button opens the two-phase manual-paste flow (`BeginManualIssueCert` →
the `_acme-challenge` paste surface + complete/cancel). The order core runs on
**all 7 apps** — native (`instant-acme`/`rcgen`) and **web** (the wasm-safe
`acme_pure` twin over RustCrypto), so web's button is **enabled**
(it issues natively too); the seal target is the connected nest's own id.

**Scope caveat.** There is **no ACME CA in the
tier_3 env**, so the order round-trip cannot be driven here — clicking the button
reaches a CA that is not present and errors. The CA half (account → order →
DNS-01-validate → finalize → fetch, including the manual two-phase + delegated
paths) is covered by the shared machine's pebble test
(`libs/fauna-client-dns/tests/pebble_dns01.rs`, Docker-gated) + the CA-free unit
tests. So this e2e asserts **affordance presence + the persistent-machine wiring**:
the issue-button renders once per domain section, which it could not before the
persistent `Arc<DnsManagementMachine>` migration (a fresh machine per action would
drop the suspended manual order).

Mirrors `test_admin_dns_cert.py`'s seed-then-poll idiom; `nest_instance` is
session-scoped so we seed a unique domain and assert against it.
"""

import secrets
import time

import pytest

pytestmark = [pytest.mark.tier_3]

_DOMAIN = f"admincertissue-{secrets.token_hex(4)}.test"


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane (mirrors
    ``test_admin_dns_cert.py::_seed_local_domain``)."""
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
def test_admin_dns_cert_issue_button_renders_per_domain(admin_app, nest_instance):
    """Each rendered `admin-dns-domain` section carries an
    `admin-dns-cert-issue-button` (get/renew certificate). A fresh test nest has no
    CA, so the button is present but its order cannot complete in-env — we assert
    only that the affordance renders, one per domain (the CA round-trip is the
    pebble test's job)."""
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_dns()

    deadline = time.time() + 15.0
    domain_names: list[str] = []
    issue_count = 0
    while time.time() < deadline:
        domain_names = [d for d in admin_app.admin.dns_domain_names() if d]
        issue_count = admin_app.admin.cert_issue_button_count()
        if _DOMAIN in domain_names and issue_count >= len(domain_names):
            break
        time.sleep(0.5)

    err = admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"
    assert _DOMAIN in domain_names, (
        f"seeded domain {_DOMAIN!r} not found among {domain_names!r} (error: {err})"
    )
    # One get/renew-certificate button per domain section.
    assert issue_count >= len(domain_names), (
        f"expected one admin-dns-cert-issue-button per domain; domains={domain_names!r} "
        f"issue_buttons={issue_count} (error: {err})"
    )
    # No manual order is pending on a freshly-loaded page, so the complete/cancel
    # paste affordances are absent for every domain (they appear only after a
    # BeginManualIssueCert, which needs a CA → covered by the pebble test).
    idx = domain_names.index(_DOMAIN)
    assert not admin_app.admin.cert_pending_paste_present(idx), (
        "no manual cert order should be pending on a freshly-loaded admin-dns page"
    )

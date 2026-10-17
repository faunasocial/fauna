"""tier_2 — issuing from one domain's row orders every name the nest's listener
serves, witnessed once and with no app.

`tls-certificates.md` § B owns the rule: an installed client-issued certificate
becomes the nest's listener certificate, so the order covers the whole listener
name set (`fauna.tls.cert_status` `desired_sans`), not only the domain whose row
was clicked. The guarantee is kept by the shared client funnel every app calls
(`fauna_client_dns::order_san_set` inside `issue_cert`), so its catalog witness
is app-less, through the e2e harness library (`feature-catalog.md` § The two
surfaces, the client-kept `[nest]` outcome ruling); the tui walk
``test_admin_dns_cert_issue_covers_listener.py`` is the app half.

**What is real.** `fauna_ffi.harness_dns_dispatch` builds the admin DNS page's
machine the way the apps build it (`build_dns_management_machine_with_credentials`),
over the admin's own authenticated WS-RPC session, and dispatches what the page
dispatches: the credential added, the second domain opted in to managed, then
`IssueCert` for that domain — the row's button. The machine reads the nest's
`desired_sans`, orders every name its credential covers as a real ACME order
against the harness's pebble CA (the compile-gated e2e ACME directory seed,
`e2e-automation-surface-gating.md` § The ACME directory seed), and pebble
validates each DNS-01 challenge against the stack's fake name server, which
serves the TXT because the fake DNS provider mirrored it there. The certificate
is sealed to the nest, delivered over `fauna.tls.publish_cert`, and installed at
`<data dir>/acme/fullchain.pem`, which the test reads.

**Why the installed file, not the served leaf.** The session nest serves plain
HTTP — it has no listener resolver — so there is no TLS port to read a served
certificate off by SNI (``test_admin_dns_cert_auto_renew_tick.py``'s reason).
The installed file is what the listener would load.

**The expectation is the nest's own answer.** The names are not hand-listed: the
test reads `desired_sans` from the same read the client makes and gives the fake
credential a zone for every apex in it, so the order must carry exactly that
set — and checks the set names the primary, `mail.<primary>` and the second
domain, so a nest answering with a narrowed set cannot pass.

tier_2, not tier_3: the DNS provider is the e2e fake (the managed-publish
precedent, ``test_admin_dns_managed.py``).
"""

from __future__ import annotations

import secrets
from pathlib import Path

import pytest

import fauna_ffi
from helpers.local_domains import active_domains, add_local_domain, ensure_primary

pytestmark = [
    pytest.mark.tier_2,
    # Starts pebble + the fake name server for the session.
    pytest.mark.acme_pebble,
]


def _admin_client(nest_instance):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _installed_leaf(fullchain: Path):
    """``(leaf, its DNS names)`` of the nest's installed certificate, or
    ``None`` when there is none."""
    from cryptography import x509

    try:
        leaf = x509.load_pem_x509_certificate(fullchain.read_bytes())
        names = leaf.extensions.get_extension_for_class(
            x509.SubjectAlternativeName
        ).value.get_values_for_type(x509.DNSName)
    except (OSError, ValueError, x509.ExtensionNotFound):
        return None
    return leaf, names


@pytest.mark.feature("admin-dns-and-certificates")
def test_issuing_for_a_second_domain_orders_every_listener_name(nest_instance, acme_pebble):
    """Outcome 56: with a primary and a second domain the credential covers,
    issuing for the SECOND domain installs one CA-issued certificate carrying
    every name `cert_status.desired_sans` lists — the primary, `mail.<primary>`
    and the second domain among them — so mail apps on `mail.<primary>` are
    never pushed back onto the self-signed floor."""
    second = f"second-{secrets.token_hex(4)}.test"
    with _admin_client(nest_instance) as admin:
        primary = ensure_primary(admin)
        add_local_domain(admin, second)
        zones = {d["domain_name"] for d in active_domains(admin)}
        desired = admin.call("fauna.tls.cert_status", {"domains": [second]})["desired_sans"]
    assert {primary, f"mail.{primary}", second} <= set(desired), (
        f"the nest's listener set should name the primary, mail.<primary> and the "
        f"second domain: desired_sans={desired!r}"
    )
    # A zone for every apex the listener set draws on (`desired_sans[0]` is the
    # nest's own name; infra hosts sit under it), so no name is dropped for want
    # of coverage and the order must equal the nest's set.
    zones.add(desired[0])

    fullchain = Path(nest_instance["tmp_dir"]) / "acme" / "fullchain.pem"
    actions = [
        {"PutCredentials": {
            "provider_id": "cloudflare",
            "fields": [{"id": "api-token", "value": "fake-dns-ok:" + ", ".join(sorted(zones))}],
            "label": "cloudflare (e2e)",
        }},
        {"SetMode": {"domain": second, "managed": True}},
        # The row's button; the empty target is aimed at the bound nest, as
        # the apps' glue aims it.
        {"IssueCert": {"domain": second, "target_nest_id": []}},
        # Leave the account as the walk found it: the opt-in off, the
        # credential cleared.
        {"SetMode": {"domain": second, "managed": False}},
        {"ClearCredentials": {"index": 0}},
    ]
    dispatched, error = fauna_ffi.harness_dns_dispatch(
        nest_instance["url"],
        bytes(nest_instance["admin"]["signing_key"]),
        actions,
        launch_env=acme_pebble.env(),
    )
    assert error == "" and dispatched == len(actions), (
        f"the DNS walk stopped after {dispatched} of {len(actions)} actions "
        f"({[next(iter(a)) for a in actions]!r}): {error}"
    )

    # `IssueCert` returns once `fauna.tls.publish_cert` answered, and the nest
    # installs before it answers (`lan_cert::install_client_issued_cert`).
    installed = _installed_leaf(fullchain)
    assert installed is not None, f"issuing for {second} installed no certificate at {fullchain}"
    leaf, names = installed
    assert leaf.issuer != leaf.subject, "the installed certificate is CA-issued, not the floor"
    assert set(names) == set(desired), (
        f"the certificate issued for {second} must carry every listener name: "
        f"got {sorted(names)!r}, desired_sans {sorted(desired)!r}"
    )

"""tier_2 e2e: issuing a certificate from one domain's row orders every name the
nest's listener serves; `tls-certificates.md` § B owns the rule (an installed
client-issued certificate becomes the nest's listener certificate, so the order
covers the whole listener name set, not only the row clicked).

**Not the catalog witness of `admin-dns-and-certificates` outcome 56.** That
outcome is `[nest]`: the guarantee is kept by the shared client funnel every app
calls (`fauna_client_dns::order_san_set`), so its witness is app-less, through
the e2e harness library (`feature-catalog.md` § The two surfaces, the
client-kept `[nest]` outcome ruling) — that witness is
``tests/api/test_dns_cert_issue_covers_listener_api.py``. This walk is the app half — the row's
button issues for that row's domain and reaches the shared order — and the
end-to-end proof that a multi-name order validates against pebble through the
fake provider.

**What is real.** The click is the admin's (`admin-dns-cert-issue-button` on the
SECOND domain's row → `DnsAction::IssueCert`). The app reads the nest's
`fauna.tls.cert_status` `desired_sans` and orders every name its credential
covers (`fauna_client_dns::order_san_set`), as a real ACME order against the
harness's pebble CA through the compile-gated e2e ACME directory seed
(`e2e-automation-surface-gating.md` § The ACME directory seed); pebble validates
each DNS-01 challenge against the stack's fake name server, which serves the TXT
because the app's fake DNS provider published it there (an RFC 2136 UPDATE
over TCP to the seed's name server). The certificate is sealed to the nest, delivered
over `fauna.tls.publish_cert`, and installed at `<data dir>/acme/fullchain.pem`,
which the test reads.

**Why the installed file, not the served leaf.** The session nest serves plain
HTTP — it has no listener resolver — so there is no TLS port to read a served
certificate off by SNI (the same reason as
``test_admin_dns_cert_auto_renew_tick.py``). The installed file is what the
listener would load.

**The expectation is the nest's own answer.** The test does not hand-list the
names: it reads `desired_sans` from the same `fauna.tls.cert_status` read the app
makes, and gives the fake credential a zone for every apex in it, so the order
must carry exactly that set. It also checks the set names the primary,
`mail.<primary>` and the second domain, so a nest answering with a narrowed set
cannot pass.

tier_2, not tier_3: the DNS provider is the e2e fake (the managed-publish
precedent, ``test_admin_dns_managed.py``).

**Why tui alone.** tui leads; the six-app trickle-down rides on the same shared
`issue_cert`.
"""

import secrets
from pathlib import Path

import pytest

from helpers import budgets
from helpers.local_domains import active_domains, add_local_domain, ensure_primary
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_2,
    pytest.mark.tui,
    # Starts pebble + the fake name server for the session and seeds
    # FAUNA_E2E_ACME_* into every app launch.
    pytest.mark.acme_pebble,
]

# The `admin-dns-domain-mode` label of a Fauna-managed domain (i18n admin.dns.mode_*).
_LABEL_MANAGED = "Fauna-managed"


def _admin_client(nest_instance):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _error(admin_app) -> str:
    return admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"


def _index(admin_app, domain: str) -> int | None:
    names = admin_app.admin.dns_domain_names()
    return names.index(domain) if domain in names else None


def _mode_of(admin_app, domain: str) -> str | None:
    i = _index(admin_app, domain)
    modes = admin_app.admin.dns_domain_modes()
    return modes[i] if i is not None and i < len(modes) else None


def _installed_names(fullchain: Path, domain: str):
    """``(leaf, its DNS names)`` of the nest's installed certificate when it
    covers ``domain``, else ``None`` (no chain yet, or an earlier one)."""
    from cryptography import x509

    try:
        leaf = x509.load_pem_x509_certificate(fullchain.read_bytes())
        names = leaf.extensions.get_extension_for_class(
            x509.SubjectAlternativeName
        ).value.get_values_for_type(x509.DNSName)
    except (OSError, ValueError, x509.ExtensionNotFound):
        return None
    return (leaf, names) if domain in names else None


def test_issuing_from_a_second_domains_row_covers_every_listener_name(
    admin_app, nest_instance
):
    """With a primary and a second domain the credential covers,
    issuing from the SECOND domain's row installs one CA-issued certificate
    carrying every name `cert_status.desired_sans` lists — the primary,
    `mail.<primary>` and the second domain among them — so mail apps on
    `mail.<primary>` are never pushed back onto the self-signed floor."""
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

    admin_app.driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: _index(admin_app, second) is not None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{second!r} never listed among {admin_app.admin.dns_domain_names()!r} "
            f"(error: {_error(admin_app)})"
        ),
    )
    held_before = admin_app.admin.dns_credential_count()
    admin_app.admin.add_dns_credential(
        "cloudflare", {"api-token": "fake-dns-ok:" + ", ".join(sorted(zones))}
    )
    try:
        wait_until(
            lambda: admin_app.admin.dns_credential_count() > held_before,
            budgets.PROVIDER_VERIFY_S,
            diagnose=lambda: f"the sentinel credential never landed (error: {_error(admin_app)})",
        )
        admin_app.admin.toggle_domain_mode(index=_index(admin_app, second))
        wait_until(
            lambda: _mode_of(admin_app, second) == _LABEL_MANAGED,
            budgets.PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"{second!r} never read {_LABEL_MANAGED!r}: {_mode_of(admin_app, second)!r} "
                f"(error: {_error(admin_app)})"
            ),
        )

        admin_app.admin.issue_cert(_index(admin_app, second))

        # `fauna.tls.publish_cert` unsealed it and wrote it where every installed
        # listener certificate lives (`lan_cert::install_client_issued_cert`).
        fullchain = Path(nest_instance["tmp_dir"]) / "acme" / "fullchain.pem"
        leaf, names = wait_until(
            lambda: _installed_names(fullchain, second),
            budgets.PROVIDER_VERIFY_S + budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"issuing from {second}'s row installed no CA-issued certificate "
                f"covering it at {fullchain} (error: {_error(admin_app)})"
            ),
        )
        assert leaf.issuer != leaf.subject, "the installed certificate is CA-issued, not the floor"
        assert set(names) == set(desired), (
            f"the certificate issued from {second}'s row must carry every listener "
            f"name: got {sorted(names)!r}, desired_sans {sorted(desired)!r}"
        )
    finally:
        try:
            if _mode_of(admin_app, second) == _LABEL_MANAGED:
                admin_app.admin.toggle_domain_mode(index=_index(admin_app, second))
            while admin_app.admin.dns_credential_count() > 0:
                admin_app.admin.clear_dns_credential(0)
        except Exception:
            pass

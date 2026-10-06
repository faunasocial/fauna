"""tier_2 e2e: the admin `admin-dns` page's one-time CNAME renewal-delegation for
a manual-mode domain (tls-certificates.md § B tier 3, S6b).

A manual-mode domain (no covering DNS credential) can automate its `_acme-challenge`
renewals by delegating them — via a single CNAME — to a zone a held credential
*does* control. `DnsAction::DelegateRenewal` validates the held credential covers
the chosen `target_zone`, persists a `CnameDelegation` in the client-held
`DnsConfig.delegations`, and surfaces the one-time CNAME on
`snapshot.delegations`; `RemoveDelegation` is the inverse.

tier_2 (like `test_admin_dns_managed.py`): a real `fauna-nest` serves the matrix,
but the *held credential* is the **fake DNS provider** (sentinel token
`fake-dns-ok:<zone>` → verify reports `<zone>` with no network call), so we have a
credential covering a controlled zone without a real registrar. The delegated
domain itself is seeded *outside* that zone, so it stays manual (the credential
doesn't cover it) — exactly the manual-domain-delegates-to-a-controlled-zone case.

Hermetic: the credential + delegation persist in the session nest's
`fauna.state.dns`, so the test removes both in a ``finally``.
"""

import secrets

import pytest

from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_2]

# The fake provider verifies the sentinel token to this controlled zone; a held
# credential then covers it and is a valid delegation target.
_ZONE = "e2e-deleg.test"
_SENTINEL_TOKEN = f"fake-dns-ok:{_ZONE}"
# The delegated domain is seeded OUTSIDE the controlled zone → stays manual.
_DOMAIN = f"deleg-{secrets.token_hex(4)}.test"


def _seed_local_domain(nest_instance, domain: str) -> None:
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
def test_admin_dns_cert_delegate_and_remove(admin_app, nest_instance):
    """Delegate a manual domain's `_acme-challenge` renewals to a held-credential
    zone, then remove the delegation — the S6b round-trip through the shared
    `DnsManagementMachine`.

    1. With a fake-verifying credential covering `e2e-deleg.test`, delegating the
       (uncovered, manual) seeded domain succeeds: the one-time CNAME renders and
       the `admin-dns-cert-remove-delegation-button` appears.
    2. Removing the delegation reverts the domain — the delegate affordance
       (`admin-dns-cert-delegate-button`) returns.
    """
    _seed_local_domain(nest_instance, _DOMAIN)
    admin_app.driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()

    wait_until(
        lambda: _DOMAIN in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {_DOMAIN!r} not rendered on admin-dns; got "
            f"{admin_app.admin.dns_domain_names()!r}"
        ),
    )

    try:
        while admin_app.admin.dns_credential_count() > 0:
            admin_app.admin.clear_dns_credential(0)

        # A held credential covering the controlled zone — the valid delegation
        # target. (Cloudflare has a single Secret field `api-token`.)
        admin_app.admin.add_dns_credential("cloudflare", {"api-token": _SENTINEL_TOKEN})
        wait_until(
            lambda: admin_app.admin.dns_credential_count() >= 1 or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                "fake-verified credential did not seal into fauna.state.dns; "
                "is the fake DNS provider enabled before the machine is built?"
            ),
        )

        idx = admin_app.admin.dns_domain_names().index(_DOMAIN)

        # --- 1. Delegate the manual domain's renewals to the controlled zone. ---
        admin_app.admin.delegate_cert_renewal(idx)

        def _delegated():
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN not in names:
                return None
            i = names.index(_DOMAIN)
            return admin_app.admin.cert_delegation_present(i) or None

        wait_until(
            _delegated,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"delegating {_DOMAIN!r} to zone {_ZONE!r} must persist a CnameDelegation "
                f"and render the one-time CNAME + remove affordance; it did not "
                f"(error: {admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
            ),
        )

        # --- 2. Remove the delegation; the delegate affordance returns. ---
        idx = admin_app.admin.dns_domain_names().index(_DOMAIN)
        admin_app.admin.remove_cert_delegation(idx)

        def _undelegated():
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN not in names:
                return None
            i = names.index(_DOMAIN)
            scope = f"admin-dns-domain[{i}]"
            present = admin_app.driver.count("admin-dns-cert-delegate-button", scope=scope) > 0
            return present or None

        wait_until(
            _undelegated,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"removing {_DOMAIN!r}'s delegation must revert it to the delegate "
                f"affordance (admin-dns-cert-delegate-button)"
            ),
        )
    finally:
        try:
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN in names:
                i = names.index(_DOMAIN)
                if admin_app.admin.cert_delegation_present(i):
                    admin_app.admin.remove_cert_delegation(i)
            while admin_app.admin.dns_credential_count() > 0:
                admin_app.admin.clear_dns_credential(0)
        except Exception:
            pass

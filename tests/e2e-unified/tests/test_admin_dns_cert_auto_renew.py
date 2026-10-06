"""tier_2 e2e: the admin `admin-dns` page's per-domain **auto-renew** checkbox
(`admin-dns-domain-auto-renew`) — tls-certificates.md § C.3 (Slice 4 C3).

Auto-renew defaults **on** for every managed/delegated domain (the only kind a
synced client can auto-issue) and is shown **only** for those rows. Toggling it
dispatches `DnsAction::SetAutoRenew`, persisted as the opt-OUT set
`DnsConfig.auto_renew_off` — config-only, so it round-trips on every app
(web included; only the background auto-issue cadence is native-only).

tier_2 (like `test_admin_dns_cert_delegate.py`): a real `fauna-nest` serves the
matrix, but the held credential is the **fake DNS provider**, so we can delegate a
manual domain to a controlled zone without a real registrar. A *delegated* domain
is exactly a row the auto-renew checkbox renders for (`delegation.is_some()`), so
the delegation gives us a domain to exercise the checkbox on.

Hermetic: the credential + delegation persist in the session nest's
`fauna.state.dns`, so the test removes both (and re-enables auto-renew) in a
``finally``.
"""

import secrets

import pytest

from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_2]

_ZONE = "e2e-autorenew.test"
_SENTINEL_TOKEN = f"fake-dns-ok:{_ZONE}"
# Seeded OUTSIDE the controlled zone → stays manual, so we delegate it (and a
# delegated domain is one the auto-renew checkbox renders for).
_DOMAIN = f"autorenew-{secrets.token_hex(4)}.test"


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
def test_admin_dns_auto_renew_default_on_and_toggles(admin_app, nest_instance):
    """A delegated domain shows the auto-renew checkbox checked by default
    (§ C.3 default-on); toggling it off then on round-trips through
    `SetAutoRenew` / `DnsConfig.auto_renew_off`."""
    _seed_local_domain(nest_instance, _DOMAIN)
    admin_app.driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()

    wait_until(
        lambda: _DOMAIN in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {_DOMAIN!r} not rendered; got {admin_app.admin.dns_domain_names()!r}"
        ),
    )

    try:
        while admin_app.admin.dns_credential_count() > 0:
            admin_app.admin.clear_dns_credential(0)

        admin_app.admin.add_dns_credential("cloudflare", {"api-token": _SENTINEL_TOKEN})
        wait_until(
            lambda: admin_app.admin.dns_credential_count() >= 1 or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: "fake-verified credential did not seal into fauna.state.dns",
        )

        # A manual domain has no auto-renew control (can't auto-issue) …
        idx = admin_app.admin.dns_domain_names().index(_DOMAIN)
        assert not admin_app.admin.auto_renew_present(idx), (
            "a manual (undelegated) domain must NOT show the auto-renew checkbox"
        )

        # … delegate it → it becomes a managed-style row → the checkbox appears,
        # checked by default (§ C.3 default-on).
        admin_app.admin.delegate_cert_renewal(idx)

        def _checkbox_on():
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN not in names:
                return None
            i = names.index(_DOMAIN)
            if not admin_app.admin.auto_renew_present(i):
                return None
            return admin_app.admin.auto_renew_state(i) == "on" or None

        wait_until(
            _checkbox_on,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"a delegated domain must show the auto-renew checkbox checked by default; "
                f"it did not (error: "
                f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
            ),
        )

        # Toggle OFF → SetAutoRenew persists the opt-out → state flips to off.
        idx = admin_app.admin.dns_domain_names().index(_DOMAIN)
        admin_app.admin.toggle_auto_renew(idx)

        def _off():
            names = admin_app.admin.dns_domain_names()
            i = names.index(_DOMAIN)
            return admin_app.admin.auto_renew_state(i) == "off" or None

        wait_until(
            _off,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                "toggling auto-renew off must persist the opt-out (DnsConfig.auto_renew_off) "
                "and re-render the checkbox unchecked"
            ),
        )

        # Toggle back ON → opt-out cleared → state flips to on.
        idx = admin_app.admin.dns_domain_names().index(_DOMAIN)
        admin_app.admin.toggle_auto_renew(idx)

        def _on_again():
            names = admin_app.admin.dns_domain_names()
            i = names.index(_DOMAIN)
            return admin_app.admin.auto_renew_state(i) == "on" or None

        wait_until(
            _on_again,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: "toggling auto-renew back on must clear the opt-out and re-render checked",
        )
    finally:
        try:
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN in names:
                i = names.index(_DOMAIN)
                if admin_app.admin.auto_renew_present(i) and admin_app.admin.auto_renew_state(i) == "off":
                    admin_app.admin.toggle_auto_renew(i)
                if admin_app.admin.cert_delegation_present(i):
                    admin_app.admin.remove_cert_delegation(i)
            while admin_app.admin.dns_credential_count() > 0:
                admin_app.admin.clear_dns_credential(0)
        except Exception:
            pass

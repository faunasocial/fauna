"""tier_2 e2e: the admin unified-DNS page (`admin-dns`) managed-mode SUCCESS
paths — adding a DNS-provider credential that verifies, and toggling a covered
domain to Fauna-managed (which auto-publishes the record matrix).

These two success halves are **untestable in tier_3**: there is no real DNS
provider in the e2e env, so ``DnsProviderSeam::verify()`` / ``publish()`` cannot
succeed against a real registrar API. The sibling tier_3 ``test_admin_dns.py``
covers only the failure halves (no provider → nothing stored; managed toggle
rejected). This file drives the success halves through a **fake DNS provider**
wired into the shared ``DnsManagementMachine``'s
``build_dns_management_machine_with_credentials`` constructor, gated per target:
native (linux/windows) reads the ``FAUNA_DNS_PROVIDER_FAKE`` env var set in the
launch config; web has no process env, so the driver flips the wasm twin flag
(``driver.enable_dns_fake_provider()`` →
``window.__fauna_enableDnsFakeProviderForTest``) before the page builds the
machine. ``driver.enable_dns_fake_provider()`` is a no-op on native (env already
set), so the test calls it uniformly with no per-platform branch. The fake is a
*decorator* over the real provider: it intercepts only credentials carrying the
test sentinel token ``fake-dns-ok:<zone>`` (verify → those zones, publish → ok
plus one witness line in the app's own log, which is how this file sees that a
publish happened); every other token delegates to the real provider, so
``test_admin_dns.py``'s bogus-token failure path is unchanged.

tier_2 (not tier_3): a real ``fauna-nest`` still serves the
``fauna.dns.list_records`` matrix that publish consumes, but the external DNS
provider — the component under test for verify/publish — is faked. Matches the
``tests/api/test_smtp_*`` "real nest + fake DNS — mixed stack → tier_2"
precedent (``scripts/tag-test-tiers.py``).

Hermetic: a stored credential persists in the *session* nest's ``fauna.state.dns``
plane, so the test clears its credential + un-manages its domain
in a ``finally`` to keep the sibling ``count==0`` assertions in
``test_admin_dns.py`` valid regardless of run order.
"""

import secrets

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_2]

# The fake provider's verify() returns whatever zones the sentinel token names;
# we seed a domain *under* this fixed zone so the held credential covers it
# (zone_covers: domain == zone or domain.ends_with(".<zone>")).
_ZONE = "e2e-managed.test"
_DOMAIN = f"m-{secrets.token_hex(4)}.{_ZONE}"
_SENTINEL_TOKEN = f"fake-dns-ok:{_ZONE}"

# The linux `admin-dns-domain-mode` ToggleButton label (i18n admin.dns.mode_*).
_LABEL_MANAGED = "Fauna-managed"

# The line the fake provider logs for every publish it receives
# (`FAKE_DNS_PUBLISH_WITNESS` in `libs/fauna-client-dns/src/lib.rs`), followed by
# the published records' `<type> <name>` list. The fake writes nothing anywhere
# else, so this line in the app's own log (native `app.err`, the web console
# ring) is the only proof the opt-in's publish reached the provider — the mode
# label alone reads "Fauna-managed" whether or not anything was published.
_PUBLISH_WITNESS = "e2e fake DNS provider published"


def _publish_witnessed(driver, domain: str) -> bool:
    """Has the app's own log recorded a fake-provider publish of a record under
    ``domain``?"""
    return any(
        _PUBLISH_WITNESS in ln and domain in ln for ln in driver.app_stderr_text().splitlines()
    )


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane — the surface
    ``fauna.dns.list_records`` enumerates records for (mirrors
    ``test_admin_dns.py``)."""
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
def test_admin_dns_managed_publish_success(admin_app, nest_instance):
    """Add a fake-verifying DNS-provider credential, then flip a covered domain
    to Fauna-managed — the two success paths tier_3 can't reach.

    1. The credential lands in the held list (``PutCredentials.verify()`` succeeds
       via the fake → sealed into ``fauna.state.dns`` → ``admin-dns-credential-item``),
       carrying the right provider + the zone the fake reported.
    2. Toggling the seeded domain (which the credential's zone covers) to managed
       succeeds: ``SetMode`` opts in, the mode control re-renders
       ``"Fauna-managed"``, and the opt-in PUBLISHES the domain's records — the
       fake provider's witness line naming the domain lands in the app's log.
       (The publish is chained inside the shared ``SetMode``; before that, tui
       and web opted in without publishing and the label alone could not tell.)
    """
    _seed_local_domain(nest_instance, _DOMAIN)
    # Enable the fake DNS provider before the credentialed DnsManagementMachine is
    # built (it reads the gate at construction time, in admin-dns's onMount). On
    # native this is a no-op — the launch config already set FAUNA_DNS_PROVIDER_FAKE
    # — but web has no process env, so the driver flips the wasm flag here. Must
    # precede navigate_dns(): the page builds the machine on mount.
    admin_app.driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()

    # The seeded domain's row (carrying admin-dns-domain-mode) must render first
    # (the nav triggers an async list_records + verify_records round-trip).
    wait_until(
        lambda: _DOMAIN in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {_DOMAIN!r} not rendered on admin-dns; got "
            f"{admin_app.admin.dns_domain_names()!r}"
        ),
    )

    try:
        # Defensive: start from a clean held-credential list (a prior managed
        # test in the same session may have left one if its cleanup was skipped).
        while admin_app.admin.dns_credential_count() > 0:
            admin_app.admin.clear_dns_credential(0)

        # --- 1. Add a credential the fake provider verifies. ---
        # Cloudflare has a single Secret field (`api-token`); the sentinel value
        # makes the decorator's verify() return the named zone without a network
        # call. (A non-sentinel token would delegate to the real provider — see
        # test_admin_dns.py::test_admin_dns_add_credential_shows_provider_error.)
        admin_app.admin.add_dns_credential("cloudflare", {"api-token": _SENTINEL_TOKEN})

        wait_until(
            lambda: admin_app.admin.dns_credential_count() >= 1 or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                "a fake-verified credential must seal into fauna.state.dns and render "
                f"as admin-dns-credential-item; held list stayed empty (error: "
                f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'}). "
                "Is the fake DNS provider enabled before the machine is built "
                "(native: FAUNA_DNS_PROVIDER_FAKE env; web: enable_dns_fake_provider() "
                "→ the wasm enableDnsFakeProviderForTest flag)?"
            ),
        )
        assert "cloudflare" in admin_app.admin.dns_credential_providers(), (
            f"stored credential provider mismatch: "
            f"{admin_app.admin.dns_credential_providers()!r}"
        )
        assert any(_ZONE in z for z in admin_app.admin.dns_credential_zones()), (
            f"stored credential should carry the fake-reported zone {_ZONE!r}; "
            f"got {admin_app.admin.dns_credential_zones()!r}"
        )

        # --- 2. Toggle the covered domain to Fauna-managed. ---
        idx = admin_app.admin.dns_domain_names().index(_DOMAIN)
        admin_app.admin.toggle_domain_mode(index=idx)

        def _is_managed():
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN not in names:
                return None
            i = names.index(_DOMAIN)
            modes = admin_app.admin.dns_domain_modes()
            return (i < len(modes) and modes[i] == _LABEL_MANAGED) or None

        wait_until(
            _is_managed,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"toggling covered domain {_DOMAIN!r} to Fauna-managed must succeed "
                f"(SetMode opts in, the client auto-Publishes via the fake provider) "
                f"and render the mode control as {_LABEL_MANAGED!r}; got "
                f"modes={admin_app.admin.dns_domain_modes()!r} (error: "
                f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
            ),
        )

        # --- 3. The opt-in published the domain's records. ---
        # A driver that reads no app log cannot see the witness.
        if admin_app.driver.log_scope_across_relaunch() == "none":
            skip_unbuilt(
                admin_app.driver,
                surface="app-log reader in the e2e driver",
                detail=(
                    "the managed opt-in succeeded, but the fake provider's publish "
                    "witness line cannot be read without the app's log"
                ),
            )
        wait_until(
            lambda: _publish_witnessed(admin_app.driver, _DOMAIN) or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"opting {_DOMAIN!r} in to Fauna-managed must publish its records "
                f"through the provider, but the fake provider never logged "
                f"{_PUBLISH_WITNESS!r} for it (error: "
                f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'}). "
                f"App log tail:\n{admin_app.driver.app_stderr_text()[-4000:]}"
            ),
        )
    finally:
        # Hermetic: un-manage the domain + drop the credential so the session
        # nest's fauna.state.dns returns to empty (sibling count==0 tests).
        try:
            names = admin_app.admin.dns_domain_names()
            if _DOMAIN in names:
                i = names.index(_DOMAIN)
                modes = admin_app.admin.dns_domain_modes()
                if i < len(modes) and modes[i] == _LABEL_MANAGED:
                    admin_app.admin.toggle_domain_mode(index=i)
            while admin_app.admin.dns_credential_count() > 0:
                admin_app.admin.clear_dns_credential(0)
        except Exception:
            pass

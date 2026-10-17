"""tier_2 e2e: a Fauna-managed domain's certificate is issued by the app's own
background auto-renew tick, with no admin tap — `admin-dns-and-certificates`
outcome 55; `tls-certificates.md` § C.3 owns the mechanism ("any synced admin
device renews a managed or delegated domain's certificate with no admin tap").
The auto-renew SETTING beside it (default on, the toggle) is
``test_admin_dns_cert_auto_renew.py``'s.

**What is real.** The tick is the app's production loop
(`admin::spawn_auto_renew_cadence` over the shared `auto_renew_scan` /
`auto_renew_issue`); only its six-hour WAIT is raced, by the compile-gated
`auto_renew_wake` poke (convention 14, `fauna_e2e_agent::AUTO_RENEW_WAKE`). The
order is a real ACME order against the harness's pebble CA, reached through the
compile-gated e2e ACME directory seed on the managed `IssueCert` path
(`e2e-automation-surface-gating.md` § The ACME directory seed); pebble validates
the DNS-01 challenge against the stack's fake name server, which serves the TXT
because the app's fake DNS provider published it there (an RFC 2136 UPDATE
over TCP to the seed's name server). The issued certificate is sealed to the nest,
delivered over `fauna.tls.publish_cert`, and installed where the nest keeps its
listener certificate (`<data dir>/acme/fullchain.pem`), which the test reads.

**Why not the badge's "Valid".** The badge folds the certificate the nest's TLS
listener SERVES (`fauna.tls.cert_status` → `served_cert_facts`), and the session
nest serves plain HTTP — it has no listener resolver, so every domain reads the
floor whatever is installed. The served-certificate fold is the nest's own,
witnessed by `conformance_tls_cert_status.rs`; this walk witnesses the half the
outcome is about — the app renewing with no tap — up to the installed file.

tier_2, not tier_3: the DNS provider is the e2e fake (the managed-publish
precedent, ``test_admin_dns_managed.py``). Delegated domains reach the same tick
through the same `IssueCert`; their witness would need a CNAME in the fake name
server, which it does not serve, so this file walks the managed shape.

**Which apps.** tui leads; linux and android carry their own `auto_renew_wake`
leg (linux's `loop_wake::LoopWake`, android's debug `AutoRenewWake`), and the rest
of the trickle-down rides on the same shared seed once each app has its leg.
"""

import secrets
from pathlib import Path

import pytest

from helpers import budgets
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_2,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    # Starts pebble + the fake name server for the session and seeds
    # FAUNA_E2E_ACME_* into every app launch.
    pytest.mark.acme_pebble,
]

# `fauna_e2e_agent::AUTO_RENEW_WAKE` — end the current session's auto-renew wait.
AUTO_RENEW_WAKE = "auto_renew_wake"

# `fauna_client_dns::AUTO_RENEW_PASS_LOG_LINE` — every tick pass that found a
# domain due writes it, then `issued [<domains>], failed [<domains>]`.
_PASS_LINE = "auto-renew pass:"

# The `admin-dns-domain-mode` label of a Fauna-managed domain (i18n admin.dns.mode_*).
_LABEL_MANAGED = "Fauna-managed"


@pytest.fixture(autouse=True)
def _android_cannot_reach_the_acme_stack(request):
    """Android is a declared environment skip (convention 7) — the reason is
    ``helpers.acme_pebble.skip_android_until_it_reaches_the_acme_stack``'s; its
    leg (`AutoRenewWake`) is built and pinned by `AutoRenewWakeTest`."""
    from helpers.acme_pebble import skip_android_until_it_reaches_the_acme_stack

    skip_android_until_it_reaches_the_acme_stack(request)


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane (mirrors
    ``test_admin_dns_managed.py::_seed_local_domain``)."""
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
            {"domain": domain, "mta_sts_cert_mode": "expand_primary"},
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


def _badge(admin_app, domain: str) -> str:
    i = _index(admin_app, domain)
    badges = admin_app.admin.cert_statuses()
    return badges[i] if i is not None and i < len(badges) else ""


def _passes(admin_app) -> list[str]:
    return [ln for ln in admin_app.driver.app_stderr_text().splitlines() if _PASS_LINE in ln]


def _installed_leaf_for(fullchain: Path, domain: str):
    """The leaf of the nest's installed certificate chain when it covers
    ``domain``, else ``None`` (no chain yet, or another domain's)."""
    from cryptography import x509

    try:
        leaf = x509.load_pem_x509_certificate(fullchain.read_bytes())
        names = leaf.extensions.get_extension_for_class(
            x509.SubjectAlternativeName
        ).value.get_values_for_type(x509.DNSName)
    except (OSError, ValueError, x509.ExtensionNotFound):
        return None
    return leaf if domain in names else None


def _issued_by_the_tick(line: str, domain: str) -> bool:
    issued = line.split("issued [", 1)[-1].split("]", 1)[0]
    return domain in [d.strip() for d in issued.split(",")]


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_managed_domain_on_the_floor_is_issued_by_the_tick_with_no_tap(
    admin_app, nest_instance
):
    """Outcome 55. A managed domain the badge shows on the floor is renewed by
    the background tick: no issue button is pressed, the tick's own pass line
    names the domain as issued, and the nest then holds a CA-issued certificate
    covering it, installed where its listener certificate lives."""
    domain = f"autorenew-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, domain)
    admin_app.driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: _index(admin_app, domain) is not None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{domain!r} never listed among {admin_app.admin.dns_domain_names()!r} "
            f"(error: {_error(admin_app)})"
        ),
    )
    held_before = admin_app.admin.dns_credential_count()
    admin_app.admin.add_dns_credential("cloudflare", {"api-token": f"fake-dns-ok:{domain}"})
    try:
        wait_until(
            lambda: admin_app.admin.dns_credential_count() > held_before,
            budgets.PROVIDER_VERIFY_S,
            diagnose=lambda: f"the sentinel credential never landed (error: {_error(admin_app)})",
        )
        admin_app.admin.toggle_domain_mode(index=_index(admin_app, domain))
        wait_until(
            lambda: _mode_of(admin_app, domain) == _LABEL_MANAGED,
            budgets.PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"{domain!r} never read {_LABEL_MANAGED!r}: {_mode_of(admin_app, domain)!r} "
                f"(error: {_error(admin_app)})"
            ),
        )
        floor = S.admin.dns.cert.status_on_floor
        wait_until(
            lambda: floor in _badge(admin_app, domain),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the fresh domain's badge should read {floor!r}: {_badge(admin_app, domain)!r}",
        )

        passes_before = len(_passes(admin_app))
        admin_app.driver.call_command(AUTO_RENEW_WAKE)
        line = wait_until(
            lambda: next(
                (ln for ln in _passes(admin_app)[passes_before:] if _issued_by_the_tick(ln, domain)),
                None,
            ),
            budgets.PROVIDER_VERIFY_S + budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"no tick pass issued {domain}; passes since the wake: "
                f"{_passes(admin_app)[passes_before:]!r} (error: {_error(admin_app)})"
            ),
        )
        assert domain in line

        # The issued certificate reached the nest: `fauna.tls.publish_cert`
        # unsealed it and wrote it where every installed listener certificate
        # lives (`lan_cert::install_client_issued_cert` → `store_acme_material`).
        fullchain = Path(nest_instance["tmp_dir"]) / "acme" / "fullchain.pem"
        leaf = wait_until(
            lambda: _installed_leaf_for(fullchain, domain),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"the tick issued {domain} but {fullchain} holds no CA-issued "
                f"certificate covering it (error: {_error(admin_app)})"
            ),
        )
        assert leaf.issuer != leaf.subject, "the installed certificate is CA-issued, not the floor"
    finally:
        try:
            if _mode_of(admin_app, domain) == _LABEL_MANAGED:
                admin_app.admin.toggle_domain_mode(index=_index(admin_app, domain))
            while admin_app.admin.dns_credential_count() > 0:
                admin_app.admin.clear_dns_credential(0)
        except Exception:
            pass

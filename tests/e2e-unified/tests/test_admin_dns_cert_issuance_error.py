"""tier_3 e2e: the admin unified-DNS page (`admin-dns`) says why the nest's own
certificate requests are failing — one deployment-level
`admin-dns-cert-issuance-error` line per failing nest-placed ACME order, the CA's
own text inside the app's wrapper (`tls-certificates.md` § Keeping the cert
alive, *The admin sees why issuance is failing*).

The nest places its own orders only in HTTP-01 mode, which no harness session
nest runs, so the failure is recorded through the compile-gated `test-hooks`
seam `POST /api/v1/test/tls/issuance-error` — the same recorder (and clear) the
lifecycle loop's arms call, nothing else. Everything after it is production:
`acme-retry-state.json` → `fauna.tls.cert_status`'s `issuance_errors` →
`DnsAction::RefreshCertStatus` → `snapshot.cert_issuance_errors` →
`fauna_client_dns::cert_issuance_error_text` → the painted line.

The CA text is third-party: the walk records one carrying markup and a newline
and asserts the line shows the markup as plain characters and no forged second
line.
"""

import json
import re
import secrets
import urllib.request

import pytest

from helpers import budgets
from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

# Apps that paint the failing-order line; each joins as its trickle-down lands.
_RENDERS_ISSUANCE_ERROR = {"tui", "linux", "web", "android"}

_DOMAIN = f"certerr-{secrets.token_hex(4)}.test"
_CA_DETAIL = (
    f"urn:ietf:params:acme:error:connection: mail.{_DOMAIN}: "
    "<b>Timeout</b> during connect\nFORGED: all certificates fine"
)
# The formatter's plain-line rendering of `_CA_DETAIL`: the newline becomes one
# space, the markup stays as plain characters.
_SHOWN_DETAIL = _CA_DETAIL.replace("\n", " ")


def _record(url: str, order: str, detail: str | None) -> None:
    """Record (``detail``) or clear (``None``) ``order``'s last failure through
    the `test-hooks` seam."""
    req = urllib.request.Request(
        f"{url}/api/v1/test/tls/issuance-error",
        data=json.dumps({"order": order, "detail": detail}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=budgets.RPC_ROUNDTRIP_S) as resp:
        assert resp.status == 200, f"issuance-error hook returned {resp.status}"


def _clear_all(url: str) -> None:
    for order in ("domain", "ip_bridge"):
        _record(url, order, None)


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane (mirrors
    ``test_admin_dns_cert.py::_seed_local_domain``), so the page has a domain
    row and `RefreshCertStatus` reads the nest."""
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


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_shows_the_cas_error_for_a_failing_order(admin_app, nest_instance):
    """A failing domain order paints one line naming it, with the CA's text as
    data; the order's success (its clear) takes the line away."""
    if app_name(admin_app.driver) not in _RENDERS_ISSUANCE_ERROR:
        skip_unbuilt(
            admin_app.driver,
            surface="admin-dns-cert-issuance-error",
            detail="the failing-order line renders on tui, linux, web and android; "
                   "the rest follow in the apps trickle-down",
            tracked="",
        )

    url = nest_instance["url"]
    _seed_local_domain(nest_instance, _DOMAIN)
    _clear_all(url)
    try:
        admin = admin_app.admin
        admin.navigate_dns()

        def _diagnose() -> str:
            err = admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"
            return (
                f"domains={admin.dns_domain_names()!r} "
                f"errors={admin.cert_issuance_errors()!r} (error-message: {err})"
            )

        wait_until(
            lambda: _DOMAIN in admin.dns_domain_names(),
            budgets.UI_SETTLE_S,
            diagnose=_diagnose,
        )
        assert admin.cert_issuance_errors() == [], (
            f"no order is failing, so no line may paint: {_diagnose()}"
        )

        _record(url, "domain", _CA_DETAIL)
        admin.refresh_dns()
        lines = wait_until(
            lambda: admin.cert_issuance_errors() or None,
            budgets.UI_SETTLE_S,
            diagnose=_diagnose,
        )
        pattern = re.escape(
            S.admin.dns.cert.issuance_failed_domain(date="DATE", error=_SHOWN_DETAIL)
        ).replace("DATE", r"\d{4}-\d{2}-\d{2}")
        assert len(lines) == 1, f"one failing order → one line: {lines!r}"
        assert re.fullmatch(pattern, lines[0]), (
            f"the line must be the domain wrapper around the CA text as plain "
            f"single-line data; got {lines[0]!r}, want /{pattern}/"
        )

        _record(url, "domain", None)
        admin.refresh_dns()
        wait_until(
            lambda: admin.cert_issuance_errors() == [],
            budgets.UI_SETTLE_S,
            diagnose=_diagnose,
        )
    finally:
        _clear_all(url)

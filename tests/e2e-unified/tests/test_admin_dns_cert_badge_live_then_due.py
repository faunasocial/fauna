"""tier_2 e2e: a managed domain's certificate badge reads **live** once a trusted
certificate is served for it and **due** once that certificate comes inside the
renewal lead, and its **Auto-renew** starts checked because the nest holds the
domain's DNS credential — `admin-dns-and-certificates` outcome 70;
`tls-certificates.md` § B (the `admin-dns-cert-status` badge and its three
states) and § C.3 (`admin-dns-domain-auto-renew` defaults on for every
managed/delegated domain).

**Why a dedicated TLS nest.** The badge is the nest's own reading of the leaf
its listener serves for the domain (`fauna.tls.cert_status` →
`acme::cert_health_state`); the session nest serves plain HTTP, has no listener
resolver and so reports every domain on the floor (the reason
``test_admin_dns_cert_auto_renew_tick.py`` reads the installed file instead). A
function-scoped nest serving TLS is the only one whose badge can read anything
else — and the test makes it primary-domain and certificate changes no other
test should inherit.

**What is real.** The admin's own gestures, in the app: a DNS credential added
through `admin-dns-add-credential-*` (the e2e fake provider, covering every
zone of the listener's names), the row flipped to Fauna-managed, the
certificate ordered with `admin-dns-cert-issue-button` against the harness's
pebble CA through the ACME directory seed, sealed to the nest and installed at
``<data dir>/acme/fullchain.pem``, which the listener hot-reloads.

**The one harness step: time passing.** A certificate nearing its end is a fact
of the clock, not an admin gesture, and the nest has no clock seam. So the
harness lands a CA-issued chain covering the same names with 10 days left
exactly where an issuance lands one (the cert-honesty tests' shape,
``tests/platform/docker/helpers.make_ca_issued_leaf_pem``), and the badge must
then read due.

**web's walk presses the same Issue.** Its order runs in wasm and takes the
seed through its own reading (`e2e-gated-seeds.md` § The ACME directory seed →
*The web order*): the web driver writes the directory into the page's
`localStorage` (again after `serve_from`), the browser orders from pebble
directly, and the TLS nest — started with the harness's nest seed — polls the
fake name server for the propagation gate and mirrors the fake provider's
challenge into it (`fauna.dns.e2e_mirror_txt`). Its page is the nest's own
`/app/`, because `_serve_spa_proxy` cannot carry a TLS nest's WebSocket, so it
is its own test over its own fixture.
"""

from pathlib import Path

import pytest

from helpers import budgets
from helpers.local_domains import active_domains, ensure_primary
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_2

# The `admin-dns-domain-mode` label of a Fauna-managed domain (i18n admin.dns.mode_*).
_LABEL_MANAGED = "Fauna-managed"
# Inside the nest's 30-day renewal lead (`CERT_RENEWAL_LEAD_SECS`).
_DUE_DAYS_LEFT = 10


@pytest.fixture
def cert_badge_tls_nest(request, app, nest_mode, tmp_path_factory):
    """A fresh nest that serves TLS, so its cert-status reads come off a real
    listener resolver."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "cert-badge-tls", serve_tls=True,
    )
    try:
        assert nest["url"].startswith("https://"), (
            f"serve_tls nest must report an https url, got {nest['url']!r}"
        )
        yield nest
    finally:
        cleanup()


@pytest.fixture
def cert_badge_served_nest(request, app, nest_mode, tmp_path_factory, static_dir):
    """``cert_badge_tls_nest`` serving the SPA build itself: web's page and API
    on the one origin of a TLS nest."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "cert-badge-served",
        serve_tls=True, static_dir=static_dir,
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def cert_badge_served_origin(cert_badge_served_nest):
    """The ``node_url`` of a page the nest serves: the nest's own origin."""
    return cert_badge_served_nest["url"]


@pytest.fixture
def cert_badge_served_admin_app(request, app, cert_badge_served_nest):
    """``(app, nest)``: web on the nest's own ``/app/``, signed in as its admin."""
    from conftest import _login_admin_as

    restore = app.driver.serve_from(cert_badge_served_nest["url"])
    try:
        _login_admin_as(app, request, cert_badge_served_nest,
                        spa_url_fixture="cert_badge_served_origin",
                        fixture_name="cert_badge_served_admin_app")
        yield app, cert_badge_served_nest
    finally:
        restore()


@pytest.fixture
def cert_badge_spa_url(spa_proxy_for, cert_badge_tls_nest):
    """web's SPA proxy → ``cert_badge_tls_nest`` (the session ``spa_url`` proxies
    only the session nest). Resolved lazily by ``_login_admin_as``, so a native
    run never needs the SPA build."""
    return spa_proxy_for(cert_badge_tls_nest["url"])


@pytest.fixture
def cert_badge_admin_app(request, app, cert_badge_tls_nest):
    """``(app, nest)``: ``app`` signed in as the admin of ``cert_badge_tls_nest``."""
    from conftest import _login_admin_as

    _login_admin_as(app, request, cert_badge_tls_nest,
                    spa_url_fixture="cert_badge_spa_url",
                    fixture_name="cert_badge_admin_app")
    return app, cert_badge_tls_nest


def _admin_client(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _error(app) -> str:
    return app.driver.get_text("error-message") if app.has_error() else "none"


def _index(app, domain: str) -> int | None:
    names = app.admin.dns_domain_names()
    return names.index(domain) if domain in names else None


def _mode_of(app, domain: str) -> str | None:
    i = _index(app, domain)
    modes = app.admin.dns_domain_modes()
    return modes[i] if i is not None and i < len(modes) else None


def _badge(app, domain: str) -> str:
    i = _index(app, domain)
    if i is None:
        return ""
    return app.driver.get_text("admin-dns-cert-status", index=i) or ""


def _badge_after_reload(app, primary: str, want: str) -> bool:
    if want in _badge(app, primary):
        return True
    # The listener hot-reloads the chain a moment after it lands; leaving and
    # re-opening the page re-reads `fauna.tls.cert_status`.
    app.admin.navigate_dashboard()
    app.admin.navigate_dns()
    return want in _badge(app, primary)


def _land_chain(nest, desired, *, days_left: int) -> None:
    """A CA-issued chain covering ``desired`` lands exactly where an issuance
    lands one, and the listener hot-reloads it."""
    from tests.platform.docker.helpers import make_ca_issued_leaf_pem

    acme = Path(nest["tmp_dir"]) / "acme"
    chain, key = make_ca_issued_leaf_pem(list(desired), days_left=days_left)
    # Key first, then chain: the watcher reloads on the chain's write.
    (acme / "privkey.pem").write_bytes(key)
    (acme / "fullchain.pem").write_bytes(chain)


def _manage_the_primary_domain(app, nest) -> tuple[str, list[str]]:
    """The admin's gestures up to the certificate: a DNS credential, the row
    flipped to Fauna-managed, and Auto-renew found checked with no tap. Returns
    ``(primary, desired_sans)``."""
    with _admin_client(nest) as admin:
        primary = ensure_primary(admin)
        zones = {d["domain_name"] for d in active_domains(admin)}
        desired = admin.call("fauna.tls.cert_status", {"domains": [primary]})["desired_sans"]
    # A zone for every apex the listener's names draw on, so an order covers
    # them all and the installed certificate is the listener's for `primary`.
    zones.add(desired[0])

    app.driver.enable_dns_fake_provider()
    app.admin.navigate_dns()
    wait_until(
        lambda: _index(app, primary) is not None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{primary!r} never listed among {app.admin.dns_domain_names()!r} "
            f"(error: {_error(app)})"
        ),
    )
    live = S.admin.dns.cert.status_valid
    assert live not in _badge(app, primary), (
        f"a fresh nest serves its floor, so the badge must not read live yet: "
        f"{_badge(app, primary)!r}"
    )

    app.admin.add_dns_credential(
        "cloudflare", {"api-token": "fake-dns-ok:" + ", ".join(sorted(zones))}
    )
    wait_until(
        lambda: app.admin.dns_credential_count() >= 1,
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: f"the sentinel credential never landed (error: {_error(app)})",
    )
    app.admin.toggle_domain_mode(index=_index(app, primary))
    wait_until(
        lambda: _mode_of(app, primary) == _LABEL_MANAGED,
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"{primary!r} never read {_LABEL_MANAGED!r}: {_mode_of(app, primary)!r} "
            f"(error: {_error(app)})"
        ),
    )

    # § C.3: a managed domain's auto-renew is on before anyone touches it.
    def _auto_renew_on():
        i = _index(app, primary)
        return i is not None and app.admin.auto_renew_present(i) and (
            app.admin.auto_renew_state(i) == "on"
        )

    wait_until(
        _auto_renew_on,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"a managed domain must show Auto-renew checked by default; present="
            f"{app.admin.auto_renew_present(_index(app, primary))!r} state="
            f"{app.admin.auto_renew_state(_index(app, primary))!r} (error: {_error(app)})"
        ),
    )
    return primary, desired


def _assert_badge_reads_live(app, primary: str) -> None:
    live = S.admin.dns.cert.status_valid
    wait_until(
        lambda: _badge_after_reload(app, primary, live),
        budgets.PROVIDER_VERIFY_S + budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"with a trusted certificate in place, {primary!r}'s badge should read "
            f"{live!r}: {_badge(app, primary)!r} (error: {_error(app)})"
        ),
    )


def _time_passes_and_the_badge_reads_due(app, nest, primary: str, desired) -> None:
    """Time passes: the served certificate now has 10 days left."""
    live = S.admin.dns.cert.status_valid
    due = S.admin.dns.cert.status_expiring
    _land_chain(nest, desired, days_left=_DUE_DAYS_LEFT)
    wait_until(
        lambda: _badge_after_reload(app, primary, due),
        budgets.RPC_ROUNDTRIP_S * 4,
        diagnose=lambda: (
            f"with {_DUE_DAYS_LEFT} days left, {primary!r}'s badge should read "
            f"{due!r}: {_badge(app, primary)!r} (error: {_error(app)})"
        ),
    )
    assert live not in _badge(app, primary), (
        f"a due certificate must not also read live: {_badge(app, primary)!r}"
    )


# Starts pebble + the fake name server for the session and seeds FAUNA_E2E_ACME_*
# into every app launch. The live leg orders against pebble through the e2e ACME
# directory seed; web walks the same outcome in its own test below, over a page
# its nest serves (feature-catalog.md § Cell semantics, the marked-witness rule).
@pytest.mark.acme_pebble
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.tui
@pytest.mark.feature("admin-dns-and-certificates")
def test_a_managed_domains_badge_reads_live_then_due_and_auto_renew_starts_on(
    cert_badge_admin_app,
):
    """A Fauna-managed domain (the nest holds its DNS credential) shows
    Auto-renew checked with no tap; after the admin issues its certificate the
    badge reads live; once the served certificate is inside the renewal lead
    the badge reads due."""
    app, nest = cert_badge_admin_app
    primary, desired = _manage_the_primary_domain(app, nest)

    # Issue → installed → hot-reloaded → the badge reads live.
    app.admin.issue_cert(_index(app, primary))
    _assert_badge_reads_live(app, primary)

    _time_passes_and_the_badge_reads_due(app, nest, primary, desired)


# The same pebble stack: the web driver writes its directory into the page, and
# the harness seeds the name server into the TLS nest's own environment.
@pytest.mark.acme_pebble
@pytest.mark.web
@pytest.mark.feature("admin-dns-and-certificates")
def test_on_web_a_managed_domains_badge_reads_live_then_due_and_auto_renew_starts_on(
    cert_badge_served_admin_app,
):
    """Web's walk of the same outcome, on the nest's own `/app/`: the same
    default, the same Issue, the same badge (the module docstring says how
    web's order reaches pebble)."""
    app, nest = cert_badge_served_admin_app
    primary, desired = _manage_the_primary_domain(app, nest)

    # Issue → ordered from the browser → installed → the badge reads live.
    app.admin.issue_cert(_index(app, primary))
    _assert_badge_reads_live(app, primary)

    _time_passes_and_the_badge_reads_due(app, nest, primary, desired)

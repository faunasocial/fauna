"""tier_3 e2e: the admin-dns domain lifecycle and the primary-domain rename,
driven through the app the way an admin drives them — the domains are added from
the page's own add form, the rename is started, chosen and aborted from the
page's own sheet and banner (convention 8). The wire is read only to observe
what the nest holds afterwards, never to stand in for the admin.

Every test runs on its own dedicated nest (``lifecycle_nest``), never the
session ``nest_instance``: the first domain added becomes the deployment's
permanent primary and a rename flips which domain that is, so neither can happen
on a nest other tests share assertions about. A fresh claimed nest hosts no
domain, which is exactly the state the first-domain outcomes start from.

Goal docs: ``docs/goal/behavior/mail-multidomain.md`` § Primary domain cannot be
removed; ``docs/goal/behavior/mail-primary-domain-rename.md`` § Goal, § UX
surface, § Lifecycle.
"""

import secrets

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux]


@pytest.fixture
def lifecycle_nest(request, nest_mode, tmp_path_factory):
    """A dedicated, function-scoped, claimed nest that hosts no domain yet."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "dns-lifecycle")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def lifecycle_spa_url(static_dir, lifecycle_nest):
    """Function-scoped SPA proxy → ``lifecycle_nest`` (resolved only on web)."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, lifecycle_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def lifecycle_admin_app(request, app, lifecycle_nest):
    """``admin_app``, pointed at ``lifecycle_nest``."""
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        lifecycle_nest,
        spa_url_fixture="lifecycle_spa_url",
        fixture_name="lifecycle_admin_app",
    )
    yield app


def _admin_client(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _nest_domains(nest) -> list[dict]:
    """The nest's own active local-domain rows (``list_local_domains``)."""
    with _admin_client(nest) as client:
        return client.call("fauna.bridges.list_local_domains", {}).get("active", [])


def _nest_primary(nest) -> str | None:
    return next((d["domain_name"] for d in _nest_domains(nest) if d.get("is_primary")), None)


def _nest_rename(nest) -> dict | None:
    """The nest's in-flight rename, or ``None`` (``get_primary_domain_rename_status``)."""
    with _admin_client(nest) as client:
        return client.call("fauna.bridges.get_primary_domain_rename_status", {}).get("rename")


def _error_text(app) -> str | None:
    """The page's ``error-message`` text, read by count + text rather than
    visibility (a late-toggled error is not reliably re-marked visible)."""
    if app.driver.count("error-message") == 0:
        return None
    try:
        return app.driver.get_text("error-message") or None
    except Exception:
        return None


def _diagnose(app, what: str) -> str:
    return (
        f"{what}; page shows domains {app.admin.dns_domain_names()!r} "
        f"(error-message: {_error_text(app) or 'none'})"
    )


def _add_domain_through_the_page(app, nest, domain: str) -> None:
    """Add ``domain`` from the page's add form and wait until both the page and
    the nest hold it."""
    app.admin.add_domain(domain)
    wait_until(
        lambda: domain in app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, f"added domain {domain!r} never rendered"),
    )
    assert domain in [d["domain_name"] for d in _nest_domains(nest)], (
        f"the page shows {domain!r} but the nest does not hold it"
    )


def _row(app, domain: str) -> str:
    """The ``admin-dns-domain[<i>]`` scope of ``domain``'s section."""
    return f"admin-dns-domain[{app.admin.dns_domain_names().index(domain)}]"


def _two_domains(app, nest) -> tuple[str, str]:
    """Open the page on a domainless nest and add two domains through it; the
    first becomes the primary. Returns ``(primary, other)``."""
    suffix = secrets.token_hex(4)
    first, second = f"first-{suffix}.test", f"second-{suffix}.test"
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=UI_SETTLE_S)
    assert _nest_domains(nest) == [], "a fresh dedicated nest must host no domain yet"
    _add_domain_through_the_page(app, nest, first)
    _add_domain_through_the_page(app, nest, second)
    assert _nest_primary(nest) == first, (
        f"the first domain added must become the primary; the nest's primary is "
        f"{_nest_primary(nest)!r}"
    )
    return first, second


@pytest.mark.feature("admin-dns-and-certificates")
def test_primary_domain_is_marked_and_cannot_be_removed(lifecycle_admin_app, lifecycle_nest):
    """The list marks the primary, its remove control is unavailable while every
    other row's is offered, and the nest refuses removing it anyway
    (``mail-multidomain.md`` § Primary domain cannot be removed)."""
    from clients.ws_rpc_admin_client import RpcCallError

    app = lifecycle_admin_app
    primary, other = _two_domains(app, lifecycle_nest)

    wait_until(
        lambda: app.driver.count("admin-dns-domain-primary-badge", scope=_row(app, primary)) == 1
        or None,
        UI_SETTLE_S,
        diagnose=lambda: _diagnose(app, f"the primary {primary!r} row carries no primary badge"),
    )
    assert app.driver.count("admin-dns-domain-primary-badge") == 1, (
        "exactly one row is marked primary"
    )
    assert app.driver.count("admin-dns-domain-primary-badge", scope=_row(app, other)) == 0

    assert app.driver.is_disabled("admin-dns-domain-remove-button", scope=_row(app, primary)), (
        "the primary row's remove control must be unavailable"
    )
    assert app.driver.is_enabled("admin-dns-domain-remove-button", scope=_row(app, other)), (
        "a non-primary row's remove control must be offered — else the disabled "
        "read above proves nothing"
    )

    with _admin_client(lifecycle_nest) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.remove_local_domain", {"domain": primary})
    refusal = f"{excinfo.value.code} {excinfo.value}"
    assert "cannot_remove_primary_domain" in refusal, (
        f"the nest must refuse removing the primary; got {refusal!r}"
    )
    assert _nest_primary(lifecycle_nest) == primary, "the primary must survive the refusal"


@pytest.mark.feature("admin-dns-and-certificates")
def test_the_primary_row_is_the_one_that_offers_the_rename(lifecycle_admin_app, lifecycle_nest):
    """The rename is offered from the primary domain's row, and only there; a
    non-primary row offers itself as a promotion target instead
    (``mail-primary-domain-rename.md`` § UX surface)."""
    app = lifecycle_admin_app
    primary, other = _two_domains(app, lifecycle_nest)

    wait_until(
        lambda: app.driver.is_enabled("admin-dns-domain-rename-button", scope=_row(app, primary))
        or None,
        UI_SETTLE_S,
        diagnose=lambda: _diagnose(
            app, f"the primary {primary!r} row offers no enabled rename control"
        ),
    )
    assert app.driver.count("admin-dns-domain-rename-button") == 1, (
        "exactly one row offers the rename"
    )
    assert app.driver.count("admin-dns-domain-rename-button", scope=_row(app, other)) == 0
    assert app.driver.count("admin-dns-domain-promote-button", scope=_row(app, other)) == 1, (
        "the non-primary row offers itself as the rename's target"
    )
    assert app.driver.count("admin-dns-domain-promote-button", scope=_row(app, primary)) == 0


def _start_rename_from_the_sheet(app, target: str, grace_days: str) -> None:
    """Open the rename sheet from the primary row, pick ``target``, type
    ``grace_days`` (``""`` leaves the field blank) and submit."""
    app.driver.click("admin-dns-domain-rename-button")
    wait_until(
        lambda: app.admin.rename_sheet_open() or None,
        UI_SETTLE_S,
        diagnose=lambda: _diagnose(app, "the rename control never opened the sheet"),
    )
    app.driver.select("admin-dns-rename-new-primary-select", target)
    app.driver.clear_and_type("admin-dns-rename-grace-days-input", grace_days)
    app.driver.click("admin-dns-rename-submit-button")


def _abort_from_the_banner(app, nest) -> None:
    """Abort the in-flight rename through the banner's reveal-then-confirm pair
    and wait until the nest holds no rename and the banner is gone."""
    app.driver.click("admin-dns-rename-abort-button")
    app.driver.wait_for("admin-dns-rename-abort-confirm-button", timeout=UI_SETTLE_S)
    app.driver.click("admin-dns-rename-abort-confirm-button")
    wait_until(
        lambda: _nest_rename(nest) is None or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, f"abort left the rename {_nest_rename(nest)!r}"),
    )
    wait_until(
        lambda: (not app.admin.rename_banner_present()) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "the rename banner outlived the abort"),
    )


def _wait_for_rename(app, nest) -> dict:
    rename = wait_until(
        lambda: _nest_rename(nest),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "submitting the rename sheet started no rename"),
    )
    wait_until(
        lambda: app.admin.rename_banner_present() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, f"the nest holds rename {rename!r} but no banner shows"),
    )
    return rename


@pytest.mark.feature("admin-dns-and-certificates")
def test_rename_grace_period_is_chosen_at_the_start(lifecycle_admin_app, lifecycle_nest):
    """Starting a rename, the admin chooses how long both domains keep working:
    left blank it is seven days, a chosen value in one to thirty is what the
    rename runs with, and a value outside that range starts nothing
    (``mail-primary-domain-rename.md`` § Goal)."""
    app = lifecycle_admin_app
    primary, other = _two_domains(app, lifecycle_nest)

    _start_rename_from_the_sheet(app, other, "")
    rename = _wait_for_rename(app, lifecycle_nest)
    assert rename["grace_days"] == 7, f"a blank grace period must be seven days; got {rename!r}"
    _abort_from_the_banner(app, lifecycle_nest)

    _start_rename_from_the_sheet(app, other, "3")
    rename = _wait_for_rename(app, lifecycle_nest)
    assert rename["grace_days"] == 3, f"the chosen grace period must be kept; got {rename!r}"
    _abort_from_the_banner(app, lifecycle_nest)

    _start_rename_from_the_sheet(app, other, "31")
    err = wait_until(
        lambda: _error_text(app),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "a 31-day grace period surfaced no refusal"),
    )
    assert _nest_rename(lifecycle_nest) is None, (
        f"a 31-day grace period must start no rename (the page said {err!r})"
    )
    assert not app.admin.rename_banner_present()
    assert _nest_primary(lifecycle_nest) == primary


@pytest.mark.feature("admin-dns-and-certificates")
def test_an_in_flight_rename_shows_its_banner_and_aborts_from_it(
    lifecycle_admin_app, lifecycle_nest
):
    """While a rename runs the page shows it — the banner, and the primary row
    stating which domain it is becoming — and abort asks to confirm before it
    acts; confirming ends the rename with the old domain still the primary
    (``mail-primary-domain-rename.md`` § UX surface, § Lifecycle).

    The pre-flip abort exit; the grace-period exits — extend and finish early —
    are the journey below, which passes the certificate step through a test hook."""
    app = lifecycle_admin_app
    primary, other = _two_domains(app, lifecycle_nest)

    _start_rename_from_the_sheet(app, other, "")
    _wait_for_rename(app, lifecycle_nest)
    state = wait_until(
        lambda: app.driver.get_text("admin-dns-domain-rename-state", scope=_row(app, primary))
        or None,
        UI_SETTLE_S,
        diagnose=lambda: _diagnose(app, "the primary row never stated the rename in flight"),
    )
    assert other in state, f"the primary row must name the rename's target; got {state!r}"
    assert app.driver.count("admin-dns-domain-rename-button", scope=_row(app, primary)) == 1
    assert app.driver.is_disabled("admin-dns-domain-rename-button", scope=_row(app, primary)), (
        "a second rename cannot be started while one is in flight"
    )

    # Abort is reveal-then-confirm: the first press dispatches nothing.
    app.driver.click("admin-dns-rename-abort-button")
    app.driver.wait_for("admin-dns-rename-abort-confirm-button", timeout=UI_SETTLE_S)
    assert _nest_rename(lifecycle_nest) is not None, "the reveal press must not abort"
    app.driver.click("admin-dns-rename-abort-confirm-button")
    wait_until(
        lambda: _nest_rename(lifecycle_nest) is None or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "confirming the abort left the rename in flight"),
    )
    wait_until(
        lambda: (not app.admin.rename_banner_present()) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "the rename banner outlived the abort"),
    )
    assert _nest_primary(lifecycle_nest) == primary, "aborting keeps the old domain primary"


def _issue_the_rename_cert(nest) -> None:
    """Stand in for the CA: ``POST /api/v1/test/primary-domain-rename/cert-issued``
    stamps the in-flight rename ``cert_ready`` and runs the cert loop's own
    post-cert steps, so the rename flips into ``grace`` (``rename_cert_test_hook``).

    A standalone test nest listens on loopback and spawns no ACME loop, so no CA
    can ever issue ``mail.<new>`` here; the issuance itself is pinned by the
    nest's cert-loop unit tests and the tier_4 pebble journey. Everything from
    the flip on is the production code path."""
    import urllib.request

    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/primary-domain-rename/cert-issued",
        data=b"",
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"the rename cert hook returned {resp.status}"


_DAY_MS = 24 * 60 * 60 * 1000


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_rename_in_its_grace_period_extends_and_finishes_early_from_the_banner(
    lifecycle_admin_app, lifecycle_nest
):
    """Once a rename has flipped, the banner offers the grace-period exits: extend
    moves the end of the grace period out by the days typed, and finishing early
    asks to confirm before it acts — confirming completes the rename with the new
    domain the primary (``mail-primary-domain-rename.md`` § UX surface,
    § Lifecycle). With the abort journey above, this covers all three exits.

    The certificate step is passed by a test hook standing in for the CA
    (``_issue_the_rename_cert``); every admin act is the page's own. The cost
    texts of the confirms are untagged chrome and are not read here — what is
    pinned is that no exit dispatches before its confirm."""
    app = lifecycle_admin_app
    primary, other = _two_domains(app, lifecycle_nest)

    _start_rename_from_the_sheet(app, other, "3")
    _wait_for_rename(app, lifecycle_nest)
    assert app.driver.count("admin-dns-rename-extend-button") == 0, (
        "before the flip the banner must not offer extend"
    )
    _issue_the_rename_cert(lifecycle_nest)
    rename = _nest_rename(lifecycle_nest)
    assert rename and rename["state"] == "grace", f"the hook must flip the rename; got {rename!r}"
    assert _nest_primary(lifecycle_nest) == other, "the flip makes the new domain the primary"
    ends_before = rename["grace_ends_at"]

    def _extend_offered() -> bool:
        app.admin.refresh_dns()
        return app.driver.count("admin-dns-rename-extend-button") == 1

    wait_until(
        _extend_offered,
        RPC_ROUNDTRIP_S,
        interval=1.0,
        diagnose=lambda: _diagnose(app, "the banner never offered extend in the grace period"),
    )

    app.driver.clear_and_type("admin-dns-rename-extend-days-input", "2")
    app.driver.click("admin-dns-rename-extend-button")

    def _extended() -> dict | None:
        r = _nest_rename(lifecycle_nest)
        return r if r and r["grace_ends_at"] != ends_before else None

    extended = wait_until(
        _extended,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "extending left the grace period where it was"),
    )
    assert extended["grace_ends_at"] - ends_before == 2 * _DAY_MS, (
        f"extend must move the end out by the two days typed; "
        f"{ends_before} -> {extended['grace_ends_at']}"
    )
    assert extended["state"] == "grace"

    # Finishing early is reveal-then-confirm: the first press dispatches nothing.
    app.driver.click("admin-dns-rename-complete-button")
    app.driver.wait_for("admin-dns-rename-complete-confirm-button", timeout=UI_SETTLE_S)
    still = _nest_rename(lifecycle_nest)
    assert still and still["state"] == "grace", f"the reveal press must not complete; {still!r}"
    app.driver.click("admin-dns-rename-complete-confirm-button")
    wait_until(
        lambda: _nest_rename(lifecycle_nest) is None or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(
            app, f"confirming finish-early left the rename {_nest_rename(lifecycle_nest)!r}"
        ),
    )
    wait_until(
        lambda: (not app.admin.rename_banner_present()) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, "the rename banner outlived the completion"),
    )
    assert _nest_primary(lifecycle_nest) == other, "a completed rename keeps the new primary"
    assert primary in [d["domain_name"] for d in _nest_domains(lifecycle_nest)], (
        "the old primary stays on as an additional domain"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_after_a_rename_the_profile_shows_the_handle_on_the_new_primary(
    lifecycle_admin_app, lifecycle_nest
):
    """After a rename has flipped, a member's own profile shows their handle on
    the NEW primary domain — the display follows the primary, read when it is
    shown, with no new alias claimed (``mail-primary-domain-rename.md``
    § Display-handle interaction).

    The member is the admin, who publishes no display name, so ``profile-handle``
    shows the handle fallback. The rename is the page's own; the certificate step
    is the CA stand-in (``_issue_the_rename_cert``). The app is then closed and
    opened again as the same device: its launch sign-in is answered by the nest
    with the domain it now holds, and the header must say so."""
    from helpers.admin_wire import admin_user
    from helpers.app_surface import app_name, skip_unbuilt
    from helpers.relaunch import relaunch, require_relaunchable

    app = lifecycle_admin_app
    if app_name(app.driver) not in ("tui", "web", "linux", "android"):
        skip_unbuilt(
            app.driver,
            surface="profile-handle as <handle>@<current primary>",
            detail="the SELF header falls back to the bare handle (or the actor id) "
                   "and never reads the account's live domain",
            tracked="",
        )
    require_relaunchable(app.driver)
    handle = admin_user(lifecycle_nest).get("handle")
    assert handle, "the claimed admin must hold a handle for the header to show"
    primary, other = _two_domains(app, lifecycle_nest)

    _start_rename_from_the_sheet(app, other, "3")
    _wait_for_rename(app, lifecycle_nest)
    _issue_the_rename_cert(lifecycle_nest)
    assert _nest_primary(lifecycle_nest) == other, "the flip makes the new domain the primary"

    relaunch(app.driver)
    app.profile.navigate()
    expected = f"{handle}@{other}"
    assert app.profile.wait_for_handle(expected, timeout=RPC_ROUNDTRIP_S), (
        f"after the rename the profile header must read {expected!r} (the old "
        f"primary was {primary!r}); got {app.profile.handle_text()!r}, "
        f"error={_error_text(app) or 'none'}"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_catch_all_is_cleared_when_its_member_is_succeeded(lifecycle_admin_app, lifecycle_nest):
    """When the catch-all member's account passes to a successor identity, the
    catch-all is cleared rather than followed, and the domain's row says why it
    now reads none (``mail-multidomain.md`` § Catch-all designation and account
    succession).

    The admin's acts are UI (adding the domain, designating the catch-all,
    refreshing the page). The succession is the MEMBER's act, not the admin's,
    so it is arranged outside the admin's app through the shared-Rust signer
    (``helpers/succession.py``; convention 8's carve-out)."""
    from conftest import _make_user
    from helpers.admin_wire import admin_client, catch_all_actor_for, picker_option
    from helpers.succession import register_recovery_kit, succeed_identity
    from i18n.strings import S

    app = lifecycle_admin_app
    domain = f"catchall-{secrets.token_hex(4)}.test"
    member = _make_user(lifecycle_nest)
    with admin_client(lifecycle_nest) as client:
        row = next(
            u for u in client.users_list_all()
            if bytes(u["actor_id"]).hex() == member["actor_id_hex"]
        )
    option = picker_option(row)

    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=UI_SETTLE_S)
    _add_domain_through_the_page(app, lifecycle_nest, domain)
    # The picker's options load with the page's actor list; re-read the page so
    # the member registered above is among them.
    app.admin.refresh_dns()
    idx = app.admin.dns_domain_names().index(domain)
    app.admin.set_catch_all(option, index=idx)
    wait_until(
        lambda: catch_all_actor_for(lifecycle_nest, domain),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: _diagnose(app, f"designating {option!r} as catch-all never persisted"),
    )
    assert app.driver.count("admin-dns-domain-catch-all-cleared-state") == 0, (
        "no succession has happened yet, so the cleared notice must be absent"
    )

    kit_secret = register_recovery_kit(
        lifecycle_nest["url"],
        actor_id_hex=member["actor_id_hex"],
        identity_seed_hex=bytes(member["signing_key"]).hex(),
    )
    succeed_identity(
        lifecycle_nest["url"],
        old_actor_id_hex=member["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        old_seed_hex=bytes(member["signing_key"]).hex(),
    )
    wait_until(
        lambda: catch_all_actor_for(lifecycle_nest, domain) is None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "the nest followed or kept the catch-all across the succession",
    )

    def _cleared_shown() -> bool:
        app.admin.refresh_dns()
        return (
            app.driver.count(
                "admin-dns-domain-catch-all-cleared-state", scope=_row(app, domain)
            )
            == 1
        )

    wait_until(
        _cleared_shown,
        RPC_ROUNDTRIP_S,
        interval=1.0,
        diagnose=lambda: _diagnose(app, f"{domain!r}'s row never said its catch-all was cleared"),
    )
    assert app.admin.catch_all_selected(
        index=app.admin.dns_domain_names().index(domain)
    ) == S.admin.dns.catch_all_none, "the picker must read none after the succession"

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
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.dns_recheck import require_dns_background_recheck
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
# the published records' `<type> <name> <value>` list. The fake writes nothing anywhere
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


# ── Managed-domain journeys beyond the opt-in (tui leads; the cross-app lift
#    is a separate row). Each seeds its OWN domains, so the per-app
#    parametrizations of the session nest never share state. ──

# The line the fake provider logs for every withdrawal it receives
# (`FAKE_DNS_WITHDRAW_WITNESS` in `libs/fauna-client-dns/src/lib.rs`), followed
# by each withdrawn `<type> <name> <value>`.
_WITHDRAW_WITNESS = "e2e fake DNS provider withdrew"

# The reason the refusing sentinel (`fake-dns-refuse:<zone>`) gives for every
# publish (`FAKE_DNS_REFUSAL`) — what the page must say.
_REFUSAL = "e2e fake DNS provider refused the write"

_LABEL_MANUAL = "Manual"


def _log_lines(driver, needle: str, *also: str) -> list[str]:
    """The app's own log lines carrying ``needle`` and every one of ``also``."""
    return [
        ln
        for ln in driver.app_stderr_text().splitlines()
        if needle in ln and all(a in ln for a in also)
    ]


def _require_log_reader(admin_app, what: str) -> None:
    if admin_app.driver.log_scope_across_relaunch() == "none":
        skip_unbuilt(
            admin_app.driver,
            surface="app-log reader in the e2e driver",
            detail=f"{what} is witnessed only by the fake provider's line in the app's log",
        )


def _error_text(admin_app) -> str | None:
    if admin_app.driver.count("error-message") == 0:
        return None
    try:
        return admin_app.driver.get_text("error-message") or None
    except Exception:
        return None


def _mode_of(admin_app, domain: str) -> str | None:
    names = admin_app.admin.dns_domain_names()
    if domain not in names:
        return None
    i = names.index(domain)
    modes = admin_app.admin.dns_domain_modes()
    return modes[i] if i < len(modes) else None


def _wait_mode(admin_app, domain: str, label: str, why: str) -> None:
    wait_until(
        lambda: _mode_of(admin_app, domain) == label or None,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"{why}: {domain!r} should read {label!r}, reads "
            f"{_mode_of(admin_app, domain)!r} (error: {_error_text(admin_app) or 'none'})"
        ),
    )


def _open_with_domains(admin_app, nest_instance, domains) -> None:
    for d in domains:
        _seed_local_domain(nest_instance, d)
    admin_app.driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: all(d in admin_app.admin.dns_domain_names() for d in domains) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domains {domains!r} not all rendered on admin-dns; got "
            f"{admin_app.admin.dns_domain_names()!r}"
        ),
    )
    while admin_app.admin.dns_credential_count() > 0:
        admin_app.admin.clear_dns_credential(0)


def _add_credential(admin_app, token: str) -> None:
    before = admin_app.admin.dns_credential_count()
    admin_app.admin.add_dns_credential("cloudflare", {"api-token": token})
    wait_until(
        lambda: admin_app.admin.dns_credential_count() > before or None,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"the sentinel credential {token!r} never landed in the held list "
            f"(error: {_error_text(admin_app) or 'none'})"
        ),
    )


def _toggle_mode(admin_app, domain: str) -> None:
    idx = admin_app.admin.dns_domain_names().index(domain)
    admin_app.admin.toggle_domain_mode(index=idx)


def _cleanup(admin_app, domains) -> None:
    """Hand every domain back to manual and drop every held credential, so the
    session nest's `fauna.state.dns` is empty again for the sibling tests."""
    try:
        if admin_app.admin.manage_all_state() == "on":
            admin_app.admin.toggle_manage_all()
        for d in domains:
            if _mode_of(admin_app, d) == _LABEL_MANAGED:
                _toggle_mode(admin_app, d)
        while admin_app.admin.dns_credential_count() > 0:
            admin_app.admin.clear_dns_credential(0)
    except Exception:
        pass


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_managed_records_republished_and_superseded_policy_taken_down(
    admin_app, nest_instance
):
    """A managed domain is looked after while the page stays open, with no tap:

    1. Public DNS does not serve the seeded domain's records (nothing does, for
       a ``.test`` name), so the page's own background re-check finds them
       missing and publishes them again — a second publish witness after the
       opt-in's, with no gesture in between (``dns-management.md``
       § Fauna-managed → Reconcile; § Manual + live verification's slow
       background cadence).
    2. The domain's DMARC policy is then softened from elsewhere — the nest's
       own admin control plane, standing in for another admin device; the app
       under test makes no gesture. The next re-check re-reads the matrix,
       publishes the new ``p=quarantine`` policy, and takes the superseded
       ``p=reject`` record down rather than leaving two DMARC records — the
       fake provider's withdrawal line names it.
    """
    require_dns_background_recheck(admin_app.driver)
    zone = f"rp-{secrets.token_hex(4)}.test"
    domain = f"m.{zone}"
    _open_with_domains(admin_app, nest_instance, [domain])
    _require_log_reader(admin_app, "a re-publish")
    try:
        _add_credential(admin_app, f"fake-dns-ok:{zone}")
        _toggle_mode(admin_app, domain)
        _wait_mode(admin_app, domain, _LABEL_MANAGED, "the covered opt-in")
        wait_until(
            lambda: _publish_witnessed(admin_app.driver, domain) or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: f"the opt-in never published {domain!r}",
        )

        # --- 1. re-published by the page itself ---
        opt_in_publishes = len(_log_lines(admin_app.driver, _PUBLISH_WITNESS, domain))
        wait_until(
            lambda: len(_log_lines(admin_app.driver, _PUBLISH_WITNESS, domain))
            > opt_in_publishes
            or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"the open page never re-published {domain!r} although public DNS "
                f"serves none of its records (publishes so far: {opt_in_publishes}; "
                f"error: {_error_text(admin_app) or 'none'}). App log tail:\n"
                f"{admin_app.driver.app_stderr_text()[-4000:]}"
            ),
        )

        # --- 2. a policy edit made elsewhere: new value out, old one down ---
        from helpers.admin_wire import admin_client

        with admin_client(nest_instance) as client:
            client.call(
                "fauna.bridges.update_local_domain_config",
                {"domain": domain, "dmarc_policy_mode": "quarantine"},
            )
        dmarc = f"_dmarc.{domain}"
        wait_until(
            lambda: _log_lines(admin_app.driver, _PUBLISH_WITNESS, dmarc, "p=quarantine")
            or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"the softened DMARC policy for {domain!r} was never published "
                f"(error: {_error_text(admin_app) or 'none'})"
            ),
        )
        wait_until(
            lambda: _log_lines(admin_app.driver, _WITHDRAW_WITNESS, dmarc, "p=reject") or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"the superseded p=reject record at {dmarc} was never taken down "
                f"(withdrawals so far: "
                f"{_log_lines(admin_app.driver, _WITHDRAW_WITNESS)!r})"
            ),
        )
        assert not _log_lines(admin_app.driver, _WITHDRAW_WITNESS, dmarc, "p=quarantine"), (
            "the current DMARC policy must never be withdrawn"
        )
    finally:
        _cleanup(admin_app, [domain])


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_failed_publish_says_why_and_stays_managed(admin_app, nest_instance):
    """A provider that accepted the credential but refuses the write (a revoked
    token, a zone moved away): opting the covered domain in fails to publish,
    the page says why — the provider's own reason, in ``error-message`` — and
    the domain still reads Fauna-managed rather than quietly turning manual
    (``dns-management.md`` § Fauna-managed: a publish failure "does not
    silently fall back to manual")."""
    zone = f"rf-{secrets.token_hex(4)}.test"
    domain = f"m.{zone}"
    _open_with_domains(admin_app, nest_instance, [domain])
    try:
        _add_credential(admin_app, f"fake-dns-refuse:{zone}")
        _toggle_mode(admin_app, domain)
        err = wait_until(
            lambda: (_error_text(admin_app) or "") if _REFUSAL in (_error_text(admin_app) or "") else None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"the refused publish never reached error-message (shows "
                f"{_error_text(admin_app)!r})"
            ),
        )
        assert _REFUSAL in err
        _wait_mode(admin_app, domain, _LABEL_MANAGED, "a failed publish keeps the opt-in")
    finally:
        _cleanup(admin_app, [domain])


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable(
    admin_app, nest_instance
):
    """The master switch and the credential behind it:

    1. With a credential covering every hosted domain, one switch hands them
       all to Fauna — and each domain's choice stays its own: handing one back
       leaves the rest managed and turns the switch off
       (``dns-management.md`` § The two modes).
    2. A domain reads managed only while a held credential covers it: removing
       the credential returns every domain to manual (§ Where the credential
       lives), and holding a covering one again brings the remembered choices
       back with no switch touched (§ The two modes — the effective mode is
       opted-in ∧ covered).
    """
    zone = f"ma-{secrets.token_hex(4)}.test"
    ours = [f"a.{zone}", f"b.{zone}"]
    _open_with_domains(admin_app, nest_instance, ours)
    every = admin_app.admin.dns_domain_names()
    # The sentinel names every hosted domain as a covered zone, so the switch
    # can manage the whole nest — the primary and other journeys' domains too.
    token = "fake-dns-ok:" + ",".join(every)
    if admin_app.admin.manage_all_state() is None:
        skip_unbuilt(
            admin_app.driver,
            surface="admin-dns-manage-all-toggle state attr",
            detail="the master switch's checked state is not mirrored for the driver",
            tracked="docs/goal/behavior/dns-management.md § Implementation status today",
        )
    try:
        _add_credential(admin_app, token)
        assert admin_app.admin.manage_all_state() == "off"

        # --- 1. one switch, every domain; choices stay per domain ---
        admin_app.admin.toggle_manage_all()
        wait_until(
            lambda: all(m == _LABEL_MANAGED for m in admin_app.admin.dns_domain_modes())
            or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                "manage-all left domains unmanaged: "
                f"{dict(zip(admin_app.admin.dns_domain_names(), admin_app.admin.dns_domain_modes()))!r} "
                f"(error: {_error_text(admin_app) or 'none'})"
            ),
        )
        wait_until(
            lambda: admin_app.admin.manage_all_state() == "on" or None,
            UI_SETTLE_S,
            diagnose=lambda: f"switch reads {admin_app.admin.manage_all_state()!r}",
        )
        _toggle_mode(admin_app, ours[0])  # hand just this one back
        _wait_mode(admin_app, ours[0], _LABEL_MANUAL, "handing one domain back")
        assert _mode_of(admin_app, ours[1]) == _LABEL_MANAGED, (
            "handing one domain back must leave the others managed"
        )
        wait_until(
            lambda: admin_app.admin.manage_all_state() == "off" or None,
            UI_SETTLE_S,
            diagnose=lambda: (
                f"with one domain manual the switch must read off; reads "
                f"{admin_app.admin.manage_all_state()!r}"
            ),
        )

        # --- 2. managed only while a covering credential is held ---
        admin_app.admin.clear_dns_credential(0)
        _wait_mode(admin_app, ours[1], _LABEL_MANUAL, "removing the only credential")
        assert all(m == _LABEL_MANUAL for m in admin_app.admin.dns_domain_modes()), (
            "with no credential held every domain reads manual: "
            f"{admin_app.admin.dns_domain_modes()!r}"
        )
        _add_credential(admin_app, token)
        _wait_mode(admin_app, ours[1], _LABEL_MANAGED, "a covering credential held again")
        assert _mode_of(admin_app, ours[0]) == _LABEL_MANUAL, (
            "the domain handed back stays manual — its own choice is remembered"
        )
    finally:
        _cleanup(admin_app, admin_app.admin.dns_domain_names())


@pytest.fixture
def admin_sibling_seats(request, nest_instance):
    """Second enrolled seats of the nest admin (`helpers.fleet.SiblingSeats`)."""
    from helpers.fleet import SiblingSeats

    seats = SiblingSeats(request, nest_instance)
    try:
        yield seats
    finally:
        seats.teardown()


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_credential_added_on_one_device_is_on_the_other(
    admin_app, nest_instance, admin_sibling_seats
):
    """A DNS-provider credential the admin adds on one device is held on their
    other device too, with nothing entered there: it rests in the admin's own
    account record, which every one of their devices reads
    (``dns-management.md`` § Where the credential lives — "any of the admin's
    clients can act on the credential"). The second device is a second
    enrolled seat of the same admin identity (convention 16)."""
    from helpers.admin_wire import admin_user
    from helpers.fleet import poked_pass

    zone = f"sd-{secrets.token_hex(4)}.test"
    domain = f"m.{zone}"
    _open_with_domains(admin_app, nest_instance, [domain])
    admin = nest_instance["admin"]
    identity = {
        "signing_key": admin["signing_key"],
        "actor_id_hex": bytes(admin["signing_key"].verify_key).hex(),
        "handle": admin_user(nest_instance).get("handle"),
    }
    try:
        _add_credential(admin_app, f"fake-dns-ok:{zone}")
        seat_b = admin_sibling_seats.launch(user=identity)
        seat_b.admin.navigate_dns()

        def held_on_b():
            poked_pass(seat_b.driver, what="device B's account walk")
            seat_b.admin.refresh_dns()
            return any(zone in z for z in seat_b.admin.dns_credential_zones()) or None

        wait_until(
            held_on_b,
            PROVIDER_VERIFY_S * 4,
            interval=2.0,
            diagnose=lambda: (
                "device B never held the credential added on device A; its list: "
                f"providers={seat_b.admin.dns_credential_providers()!r} "
                f"zones={seat_b.admin.dns_credential_zones()!r}"
            ),
        )
        assert "cloudflare" in seat_b.admin.dns_credential_providers()
    finally:
        _cleanup(admin_app, [domain])


# ── A second admin's view of a domain the first admin manages ──
#
# A DEDICATED nest: the second admin is made by `helpers.admin_wire.grant_admin`,
# whose `pending_actions/run_due` hook runs every pending action nest-wide, and an
# extra admin on the shared session nest would change who `admin_user` and every
# roster read find there.


@pytest.fixture
def two_admin_dns_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest whose claimer has granted a member the admin role:
    ``nest["second_admin"]`` is that member (a ``_make_user`` dict)."""
    from conftest import _make_user, _start_dedicated_nest
    from helpers.admin_wire import grant_admin

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "two-admin-dns")
    try:
        nest["second_admin"] = _make_user(nest)
        grant_admin(nest, nest["second_admin"])
        yield nest
    finally:
        cleanup()


@pytest.fixture
def two_admin_dns_spa_url(static_dir, two_admin_dns_nest):
    """Function-scoped SPA proxy → ``two_admin_dns_nest`` (the session ``spa_url``
    proxies only ``nest_instance``). Mirrors ``crowded_spa_url``."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, two_admin_dns_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def two_admin_dns_app(request, app, two_admin_dns_nest):
    """``admin_app``, but signed in as ``two_admin_dns_nest``'s claimer — the
    first admin (`_login_admin_as` — only the nest differs)."""
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        two_admin_dns_nest,
        spa_url_fixture="two_admin_dns_spa_url",
        fixture_name="two_admin_dns_app",
    )
    yield app


@pytest.fixture
def second_admin_seats(request, two_admin_dns_nest):
    """Seats on ``two_admin_dns_nest`` for its second admin (`helpers.fleet.SiblingSeats`)."""
    from helpers.fleet import SiblingSeats

    seats = SiblingSeats(request, two_admin_dns_nest)
    try:
        yield seats
    finally:
        seats.teardown()


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_another_admin_reads_manual_for_a_domain_the_first_manages(
    two_admin_dns_app, two_admin_dns_nest, second_admin_seats
):
    """A domain one admin manages reads manual to another admin: the opt-in and
    the credential that makes it actionable rest in the first admin's own account
    record, which the second admin's devices never read, so the second admin's
    effective mode is manual (``dns-management.md`` § The two modes — "on a
    two-admin deployment where only one admin has the provider API key, a
    nest-shared flag would falsely imply the keyless admin could auto-publish").

    Admin A, on the app under test, holds a covering credential and opts the
    domain in; admin B, on a tui seat of their own, lists the same domain — the
    record matrix is the nest's, every admin's — and reads it manual, holding no
    credential."""
    from helpers.fleet import poked_pass

    app_a, nest = two_admin_dns_app, two_admin_dns_nest
    zone = f"ta-{secrets.token_hex(4)}.test"
    domain = f"m.{zone}"
    _open_with_domains(app_a, nest, [domain])
    _add_credential(app_a, f"fake-dns-ok:{zone}")
    _toggle_mode(app_a, domain)
    _wait_mode(app_a, domain, _LABEL_MANAGED, "admin A's covered opt-in")

    seat_b = second_admin_seats.launch(user=nest["second_admin"])
    seat_b.admin.navigate_dns()

    def listed_on_b():
        # A full account walk first, so B's page projects from everything B's
        # own record holds — what A's record holds must not reach it.
        poked_pass(seat_b.driver, what="admin B's account walk")
        seat_b.admin.refresh_dns()
        return _mode_of(seat_b, domain)

    mode_b = wait_until(
        listed_on_b,
        RPC_ROUNDTRIP_S,
        interval=2.0,
        diagnose=lambda: (
            f"admin B's admin-dns never listed {domain!r}; it lists "
            f"{seat_b.admin.dns_domain_names()!r} (error: {_error_text(seat_b) or 'none'})"
        ),
    )
    assert mode_b == _LABEL_MANUAL, (
        f"a domain admin A manages must read manual to admin B, who holds no covering "
        f"credential; B reads {mode_b!r}"
    )
    assert seat_b.admin.dns_credential_count() == 0, (
        "admin A's credential rests in A's own account record and must not be held "
        f"on admin B's seat; B holds providers={seat_b.admin.dns_credential_providers()!r}"
    )
    assert _mode_of(app_a, domain) == _LABEL_MANAGED, (
        "admin B's view must not change admin A's: A still reads the domain managed"
    )

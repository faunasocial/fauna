"""The admin users, tiers and DNS pages offer only what is possible: each
journey makes a precondition unmet through the page itself and reads what the
page offers then.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot; what they
can never do here is not shown; rule 4's form case: a form's submit is painted
from its draft and the form closes only once its save has succeeded). The
per-page instances: ``docs/goal/behavior/admin.md`` § 2. Users → *What a row
offers*, § 3. Settings → *Removing a tier*, and
``docs/goal/behavior/dns-management.md`` § Errors & edge cases.

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. tui's in-process walk invariant and the shared crates' machine
tests are not that witness.

**The reason is read as painted.** ``PlatformDriver.blocked_reason`` answers
what the app shows beside the control (tui's seam publishes it only once it is
painted; linux's ``possible::paint`` beside its tooltip or reason line; web
the control's ``title``) and the journey compares it with the ``S.`` string of
the shared predicate's reason key, never a literal. An app whose driver cannot
read it yet takes the ``blocked-reason-read`` surface gate.

Arrangement through the admin control plane (a guardian account, a hosted
domain) is setup, not the act under test (convention 8); every act the
outcome names goes through the page.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.admin_wire import admit_adult, seed_local_domain
from helpers.app_surface import app_name, require_surface, surface_built
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


def _assert_blocked_with(driver, element_id: str, reason: str, *, scope: str | None = None,
                         index: int = 0, what: str) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it. One message names every part that failed (convention 6)."""
    diag = driver.diagnose(element_id, attrs=("enabled", "reason", "title"), scope=scope)
    assert not driver.is_absent(element_id, scope=scope), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id, scope=scope), (
        f"{what}: {element_id} is live while its precondition is unmet: {diag}"
    )
    painted = driver.blocked_reason(element_id, index, scope=scope)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _admin_hex(nest_instance) -> str:
    return bytes(nest_instance["admin"]["signing_key"].verify_key).hex()


# ── admin-users 42 ────────────────────────────────────────────────────────────


@pytest.mark.feature("admin-users")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_users_page_offers_no_role_removal_or_approval_the_nest_always_refuses(
    admin_app, nest_instance,
):
    """``admin.md`` § 2 → *What a row offers*: Remove admin is absent on a row
    without the role and blocked, its reason naming the step, on the nest's
    last superadmin; Approve is absent on an action the viewer scheduled.

    A fresh nest has one superadmin (a second admin joins only after a 24 h
    grant executes), so the viewer's own row is the last superadmin's. The
    viewer then schedules a grant on a plain row through the page, and that
    action — the viewer's own — must carry no Approve. The grant is called off
    afterwards through the same row."""
    driver = admin_app.driver
    require_surface(driver, "blocked-reason-read")
    admin = admin_app.admin

    plain = admit_adult(nest_instance, f"plainrow{secrets.token_hex(3)}")
    plain_hex = plain["actor_id_hex"]
    def _listed():
        admin.navigate_users()
        return admin.has_user_row(plain_hex)

    wait_until(
        _listed,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the admitted row never listed: {admin_app.error_text()!r}",
    )

    # A row without the role: no Remove admin at all.
    row = admin.user_row_index(plain_hex)
    scope = f"user-row[{row}]"
    assert driver.is_absent("admin-users-remove-admin-button", scope=scope), (
        "a row without the admin role must carry no Remove admin — a choice that "
        "can never succeed: "
        f"{driver.diagnose('admin-users-remove-admin-button', attrs=('enabled',), scope=scope)}"
    )

    # The last superadmin: Remove admin shown, blocked, its reason beside it.
    admin_row = admin.admin_row_index(_admin_hex(nest_instance))
    _assert_blocked_with(
        driver, "admin-users-remove-admin-button",
        S.admin.users_page.remove_admin_last_superadmin,
        scope=f"user-row[{admin_row}]",
        what="the nest's last superadmin",
    )

    # The viewer's own scheduled action: no Approve on it.
    row = admin.user_row_index(plain_hex)
    admin.make_admin(row)

    def _pending_row():
        admin.navigate_users()
        i = admin.pending_action_row_index(plain_hex)
        return i + 1 if i >= 0 else None

    pending = wait_until(
        _pending_row, RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the scheduled grant never listed as pending: "
            f"{admin.pending_action_descriptions()!r}, "
            f"action error {admin.users_action_error_text()!r}"
        ),
    ) - 1
    item = f"admin-pending-action-item[{pending}]"
    try:
        assert driver.is_absent("admin-pending-action-approve-button", scope=item), (
            "an action the viewer scheduled must carry no Approve — the nest refuses "
            "self-approval: "
            f"{driver.diagnose('admin-pending-action-approve-button', attrs=('enabled',), scope=item)}"
        )
    finally:
        admin.cancel_pending_action(pending)


# ── admin-users 43 ────────────────────────────────────────────────────────────


def _assert_band_waits_for_guardian(admin_app, guardian_select: str, band_select: str,
                                    guardian_handle: str, *, form: str) -> None:
    """The band select is blocked with its reason while no guardian is picked,
    and live once one is."""
    driver = admin_app.driver
    _assert_blocked_with(
        driver, band_select, S.admin.users_page.age_band_needs_guardian,
        what=f"the {form} form with no guardian picked",
    )
    wait_until(
        lambda: guardian_handle in (driver.option_texts(guardian_select) or []),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"{guardian_handle!r} never offered: {driver.option_texts(guardian_select)!r}",
    )
    driver.select(guardian_select, guardian_handle)
    driver.wait_until_enabled(band_select, timeout=10.0)


@pytest.mark.feature("admin-users")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_age_band_says_to_pick_a_guardian_first_until_one_is_picked(
    admin_app, nest_instance,
):
    """``admin.md`` § 2 → *What a row offers*: on admit and on invite the age
    band is blocked, with its reason, until a guardian is picked (the nest
    refuses a band without one — ``age_band_possibility``)."""
    driver = admin_app.driver
    require_surface(driver, "blocked-reason-read")
    admin = admin_app.admin
    guardian = f"bandguardian{secrets.token_hex(3)}"
    admit_adult(nest_instance, guardian)

    admin.navigate_users()
    driver.wait_for("admin-users-admit-age-band-select", timeout=30)
    _assert_band_waits_for_guardian(
        admin_app, "admin-users-admit-guardian-select",
        "admin-users-admit-age-band-select", guardian, form="admit",
    )

    admin.navigate_invite_codes()
    admin.open_invite_form()
    _assert_band_waits_for_guardian(
        admin_app, "admin-users-invite-guardian-select",
        "admin-users-invite-age-band-select", guardian, form="invite",
    )


# ── admin-tiers 17 ────────────────────────────────────────────────────────────


@pytest.mark.feature("admin-tiers")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_free_tier_row_carries_no_delete_control(admin_app):
    """``admin.md`` § 3 → *Removing a tier*: the nest never removes ``free``,
    so its row carries no delete control, while every other tier's row does
    (the nest seeds free, personal and community)."""
    driver = admin_app.driver
    admin = admin_app.admin
    admin.navigate_settings()
    names = wait_until(
        lambda: (lambda n: n if "free" in n and len(n) > 1 else None)(admin.tier_names()),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the tier list never loaded: {admin.tier_names()!r}, {admin_app.error_text()!r}",
    )
    for i, name in enumerate(names):
        scope = f"admin-settings-tier-item[{i}]"
        absent = driver.is_absent("admin-settings-tier-delete-button", scope=scope)
        diag = driver.diagnose("admin-settings-tier-delete-button", attrs=("enabled",), scope=scope)
        if name == "free":
            assert absent, f"the free tier's row must carry no delete control: {diag}"
        else:
            assert not absent, f"the {name!r} tier's row should offer delete: {diag}"


# ── admin-dns-and-certificates 75 ─────────────────────────────────────────────


def _open_dns_with(admin_app, domain: str) -> int:
    admin = admin_app.admin
    admin.navigate_dns()
    wait_until(
        lambda: domain in admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"{domain!r} never listed: {admin.dns_domain_names()!r}",
    )
    return admin.dns_domain_names().index(domain)


@pytest.mark.feature("admin-dns-and-certificates")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_switch_to_fauna_managed_waits_for_a_covering_credential(admin_app, nest_instance):
    """``dns-management.md`` § Errors & edge cases: while no held credential
    covers a domain its switch to Fauna-managed is disabled with the reason
    beside it, adding a credential stays offered, and the switch for all
    domains is not live either. tier_3 holds no credential (none can verify
    against a real provider), so every listed domain is uncovered."""
    driver = admin_app.driver
    require_surface(driver, "blocked-reason-read")
    admin = admin_app.admin
    domain = f"possible-{secrets.token_hex(4)}.test"
    seed_local_domain(nest_instance, domain)
    index = _open_dns_with(admin_app, domain)
    assert admin.dns_credential_count() == 0, (
        f"this journey needs no held credential; held {admin.dns_credential_providers()!r}"
    )

    _assert_blocked_with(
        driver, "admin-dns-domain-mode", S.admin.dns.mode_needs_credential,
        scope=f"admin-dns-domain[{index}]",
        what=f"{domain} with no covering credential",
    )
    assert driver.is_enabled("admin-dns-add-credential-button"), (
        "adding a credential — the step that unblocks the switch — must stay offered: "
        f"{driver.diagnose('admin-dns-add-credential-button', attrs=('enabled',))}"
    )
    _assert_blocked_with(
        driver, "admin-dns-manage-all-toggle", S.admin.dns.manage_all_needs_credential,
        what="the switch for all domains with no covering credential",
    )


# ── admin-dns-and-certificates 76 ─────────────────────────────────────────────


@pytest.mark.feature("admin-dns-and-certificates")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_dns_forms_say_why_they_cannot_save_and_keep_a_failed_draft(admin_app, nest_instance):
    """``dns-management.md`` § Errors & edge cases: the add-domain and
    add-credential forms disable their save and say why while a field is blank
    or the domain is already listed, and a form closes only once its save has
    succeeded, keeping what was typed when it fails.

    The failing save is the credential's: tier_3 reaches no real provider, so
    its verify fails (``test_admin_dns_add_credential_shows_provider_error``)."""
    driver = admin_app.driver
    require_surface(driver, "blocked-reason-read")
    admin = admin_app.admin
    domain = f"possible-{secrets.token_hex(4)}.test"
    seed_local_domain(nest_instance, domain)
    _open_dns_with(admin_app, domain)

    # Add domain: blank, then a domain already listed.
    driver.click("admin-dns-add-domain-button")
    driver.wait_for("admin-dns-add-domain-input", timeout=10.0)
    _assert_blocked_with(
        driver, "admin-dns-add-domain-submit-button", S.admin.dns.domain_required,
        what="the add-domain form left blank",
    )
    driver.type_text("admin-dns-add-domain-input", domain)
    wait_until(
        lambda: driver.blocked_reason("admin-dns-add-domain-submit-button")
        == S.admin.dns.domain_duplicate,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: driver.diagnose(
            "admin-dns-add-domain-submit-button", attrs=("enabled", "reason", "title")
        ),
    )
    _assert_blocked_with(
        driver, "admin-dns-add-domain-submit-button", S.admin.dns.domain_duplicate,
        what=f"the add-domain form holding {domain}, already listed",
    )

    # Add credential: blank fields, then a save that fails keeps the draft.
    driver.click("admin-dns-add-credential-button")
    provider_row = "admin-dns-add-credential-provider-row[cloudflare]"
    driver.wait_for(provider_row, timeout=10.0)
    driver.click(provider_row)
    driver.wait_for("api-token", timeout=10.0)
    _assert_blocked_with(
        driver, "admin-dns-add-credential-submit-button", S.admin.dns.credential_field_required,
        what="the add-credential form with its field blank",
    )
    token = f"not-a-real-token-{secrets.token_hex(6)}"
    driver.type_text("api-token", token)
    driver.wait_until_enabled("admin-dns-add-credential-submit-button", timeout=10.0)
    driver.click("admin-dns-add-credential-submit-button")

    def _err():
        if driver.count("error-message") == 0:
            return None
        return driver.get_text("error-message") or None

    err = wait_until(
        _err, PROVIDER_VERIFY_S,
        diagnose=lambda: "the failed credential save surfaced nothing on error-message",
    )
    assert admin.dns_credential_count() == 0, (
        f"a failed verify must store nothing: {admin.dns_credential_providers()!r} ({err!r})"
    )
    assert not driver.is_absent("api-token"), (
        f"the add-credential form closed although its save failed ({err!r}): "
        f"{driver.diagnose('api-token')}"
    )
    # The field is a secret, painted masked, so what was typed is read through
    # the form's own draft check: the save is live again only while every field
    # still holds text (credential_draft_possibility).
    assert driver.is_enabled("admin-dns-add-credential-submit-button"), (
        f"the failed save must keep what was typed ({err!r}): the save reads "
        "blocked, so the draft was dropped: "
        f"{driver.diagnose('admin-dns-add-credential-submit-button', attrs=('enabled', 'reason', 'title'))}"
    )


# ── admin-forwarders ──────────────────────────────────────────────────────────


@pytest.fixture
def domainless_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest of its own that hosts no local domain. The shared
    ``nest_instance`` gathers hosted domains from every test that seeds one,
    so "no hosted domain" holds only on a nest nobody else touches."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory,
                                          "domainless-nest")
    yield nest
    cleanup()


@pytest.fixture
def domainless_spa_url(static_dir, domainless_nest):
    """The web SPA proxied to ``domainless_nest`` (the shared ``spa_url``
    proxies ``nest_instance``)."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, domainless_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def domainless_admin_app(request, app, domainless_nest):
    """``app`` signed in as ``domainless_nest``'s admin, on the admin shell."""
    from conftest import _login_admin_as

    _login_admin_as(app, request, domainless_nest, spa_url_fixture="domainless_spa_url",
                    fixture_name="domainless_admin_app")
    return app


@pytest.mark.feature("admin-forwarders")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_a_forwarder_waits_for_a_hosted_domain_and_offers_the_dns_page(domainless_admin_app):
    """``admin.md`` § 4, *A forwarder needs a hosted domain*: on a nest hosting
    no local domain the forwarder add form's submit is blocked with the reason
    naming DNS, the step that opens it is offered, and pressing that step lands
    on ``admin-dns``, where a domain is added."""
    app = domainless_admin_app
    driver = app.driver
    submit = "admin-aliases-forwarder-add-submit-button"
    step = "admin-aliases-forwarder-add-domains-step"
    reason = S.admin.aliases_page.no_hosted_domain

    app.admin.navigate_aliases()
    if surface_built("blocked-reason-read", app_name(driver)):
        # The reason is painted only once the forwarder read has resolved (an
        # unread list is not an empty one), so wait for the painted reason.
        wait_until(
            lambda: not driver.is_absent(submit) and driver.blocked_reason(submit, 0) == reason,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: driver.diagnose(submit, attrs=("enabled", "reason", "title")),
        )
        _assert_blocked_with(driver, submit, reason,
                             what="the forwarder submit with no hosted domain")
    else:
        # An app whose driver cannot read the painted reason yet still owes
        # the rest of the rule: the step is offered only once the read has
        # resolved to "no hosted domain", and the submit it unblocks is not live.
        wait_until(
            lambda: not driver.is_absent(step),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: driver.diagnose(step, attrs=("enabled",)),
        )
        assert not driver.is_absent(submit) and not driver.is_enabled(submit), (
            "the forwarder submit must not be live with no hosted domain: "
            f"{driver.diagnose(submit, attrs=('enabled',))}"
        )

    assert not driver.is_absent(step) and driver.is_enabled(step), (
        "the step that unblocks the form — the DNS page — must be offered: "
        f"{driver.diagnose(step, attrs=('enabled',))}"
    )
    driver.click(step)
    wait_until(
        lambda: not driver.is_absent("admin-dns-add-domain-button"),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: driver.diagnose("admin-dns-add-domain-button", attrs=("enabled",)),
    )

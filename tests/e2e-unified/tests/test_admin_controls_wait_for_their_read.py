"""Admin controls wait for their read — one journey per admin page.

``docs/goal/behavior/admin.md`` § Errors & edge cases → *Controls wait for their
read* (ruled 2026-11-17, applying ``docs/goal/ui/README.md`` § Offer only what is
possible, rules 5–7): on every admin page a control that writes what a snapshot
read decides is not live until that read resolves — blocked with *still
loading*, or *failed to load* after a failed read — and the reason is painted
within eyeshot. Each test here is the witness of its page's catalog outcome
(``feature-catalog.md`` § The coverage contract, the possibility-outcome
bullet): it makes the precondition unmet on that page and fails when the
control is enabled, when its reason's words are not beside it, or when a press
gets through.

**The seam.** The nest is stopped (``common.nest.stop_nest``, the bridge-owned
lifecycle every offline test uses) and the app has seen the connection drop
BEFORE the page is opened, so the page's read cannot answer: the failed-read arm
(or a read still pending against a dead socket — either reason is the rule) is
reached deterministically, with no race against a live read (convention 14).
The loading arm while a live read is in flight, and the *still saving* arm
while a write is on its way, are not reachable from e2e without a timing race;
they stay with the Rust machine tests the build landed (``admin::tests``'
``*_wait_for_the_read`` cases, which also pin the press-time refusal on
``error-message``).

**The press.** A blocked control is not actuable: the automation agent refuses
a click on a disabled element (convention 11's actuation gate, answering 409
"element is disabled") exactly as the focus ring never lands on it, so nothing
is dispatched and nothing reaches the nest. Each journey presses and asserts
that refusal; the app's own second refusal (``admin::apply_local`` asking the
same gate and naming the reason on ``error-message``) is behind it.

**Non-vacuous.** After the nest comes back, each journey re-opens the page and
waits for the read reason to clear, so a control painted blocked for good — or
a reason that is not the read's — fails here too.

**One page per test, one read per launch.** A page read once in this launch
keeps its answer (a re-read that fails after one succeeded keeps the earlier
one — the rule), so each test opens a page no earlier test in this module
opened, and the module relaunches the app cold (convention 10).
"""

from __future__ import annotations


import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.app_surface import require_surface
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.connection import (
    ONLINE_BUDGET_S,
    connection_observable,
    wait_until_online,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

#: The two words the read gate paints while the read has not answered.
READ_REASONS = (S.common.still_loading, S.common.load_failed)

#: The disabled-actuation refusal's own words (``drivers/http_bridge.py``).
DISABLED_REFUSAL = "element is disabled"

#: Named generous budget for the app to see the nest gone (convention 14:
#: a green run pays one poll).
OFFLINE_SEEN_S = 90.0


def _wait_offline(app) -> None:
    def offline():
        seen = connection_observable(app.driver)
        return seen is not None and seen.get("online") is False

    wait_until(
        offline,
        OFFLINE_SEEN_S,
        diagnose=lambda: f"the app still reports {connection_observable(app.driver)!r}",
    )


def _with_nest_down(app, nest_instance, body) -> None:
    """Run ``body`` while the nest is stopped and the app has seen it go; the
    nest comes back on the same port and data whatever ``body`` did (the fixture
    is session-scoped)."""
    stop_nest(nest_instance, graceful=True)
    try:
        _wait_offline(app)
        body()
    finally:
        start_nest_in_place(nest_instance)
    wait_until_online(app.driver, timeout=ONLINE_BUDGET_S)


def _assert_blocked_by_the_read(app, element_id: str, *, index: int = 0) -> None:
    """``element_id`` is shown, disabled, says why in the read's words, and a
    press is refused before anything is dispatched."""
    driver = app.driver
    wait_until(
        lambda: driver.count(element_id) > index,
        UI_SETTLE_S,
        diagnose=lambda: f"{element_id} is not on the page. error: {app.error_text()!r}",
    )
    assert not driver.is_enabled(element_id), (
        f"{element_id} is LIVE before its read answered — a press would write over "
        f"a value the page never showed. error: {app.error_text()!r}"
    )
    reason = driver.painted_reason(element_id, index)
    assert reason in READ_REASONS, (
        f"{element_id} is disabled but says {reason!r} beside it, not that it is "
        f"still loading or failed to load ({READ_REASONS!r})"
    )
    try:
        driver.click(element_id, index)
    except RuntimeError as refused:
        assert DISABLED_REFUSAL in str(refused), (
            f"pressing {element_id} failed, but not as a refused disabled press: {refused}"
        )
    else:
        raise AssertionError(
            f"pressing {element_id} was actuated while its read had not answered"
        )


def _wait_read_reason_clears(app, navigate, element_id: str) -> None:
    """Once the nest is back and the page re-read, ``element_id`` no longer
    waits on the read: it is live, or blocked for a reason that is not the
    read's (a precondition of its own, which this outcome does not cover)."""

    def cleared():
        navigate()
        driver = app.driver
        if driver.count(element_id) == 0:
            return False
        return driver.is_enabled(element_id) or (
            driver.painted_reason(element_id) not in READ_REASONS
        )

    wait_until(
        cleared,
        RPC_ROUNDTRIP_S + ONLINE_BUDGET_S,
        interval=1.0,
        diagnose=lambda: (
            f"{element_id} still waits on its read with the nest back: "
            f"{app.driver.painted_reason(element_id)!r}. error: {app.error_text()!r}"
        ),
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("admin-nest")
def test_the_nest_pages_controls_wait_for_the_nests_settings(admin_app, nest_instance):
    """Admin → Nest: the pairing switch, the serving port's save and the
    network-mode radios and save are blocked until the reflective read and
    ``fauna.setup.status`` answer (admin.md, *Controls wait for their read*,
    Nest). A failed read leaves the NAT controls blocked: saving the
    pre-selected default over a mode the page never read would be a guess."""
    app = admin_app
    require_surface(app.driver, "painted-reason")
    controls = (
        "admin-service-pairing-toggle",
        "admin-nest-serving-port-save-button",
        "admin-nest-nat-mode-public-radio",
        "admin-nest-nat-mode-private-radio",
        "admin-nest-nat-mode-save-button",
    )

    def blocked():
        app.admin.navigate_nest()
        for element_id in controls:
            _assert_blocked_by_the_read(app, element_id)
        assert not app.driver.is_enabled("admin-nest-serving-port-input"), (
            "the serving-port input invites typing over a port the page never read"
        )

    _with_nest_down(app, nest_instance, blocked)
    _wait_read_reason_clears(app, app.admin.navigate_nest, "admin-service-pairing-toggle")


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("admin-mail-policy")
def test_the_mail_pages_controls_wait_for_the_mail_policy(admin_app, nest_instance):
    """Admin → Mail: the mail-enable and new-user auto-enable switches and every
    group's save are blocked until the policy read answers — a group's save never
    sends defaults over a policy the page has not shown (admin.md, *Controls wait
    for their read*, Mail). The rotate and publish confirms ask the same gate and
    are checked wherever the page shows them."""
    app = admin_app
    require_surface(app.driver, "painted-reason")
    controls = (
        "admin-mail-enabled-toggle",
        "admin-mail-auto-enable-new-users-toggle",
        "admin-mail-microsoft-save-button",
        "admin-mail-spam-save-button",
        "admin-mail-auth-save-button",
        "admin-mail-submission-save-button",
        "admin-mail-imap-save-button",
        "admin-mail-outbound-save-button",
        "admin-mail-alias-save-button",
        "admin-mail-dmarc-save-button",
    )
    confirms = (
        "admin-mail-publish-spam-baseline-button",
        "admin-mail-list-unsubscribe-secret-rotate-button",
        "admin-mail-forward-srs-secret-rotate-button",
    )

    def blocked():
        app.admin.navigate_mail()
        for element_id in controls:
            _assert_blocked_by_the_read(app, element_id)
        for element_id in confirms:
            if app.driver.count(element_id):
                _assert_blocked_by_the_read(app, element_id)

    _with_nest_down(app, nest_instance, blocked)
    _wait_read_reason_clears(
        app, app.admin.navigate_mail, "admin-mail-auto-enable-new-users-toggle"
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("admin-calendar-contacts-files")
def test_the_dav_switches_wait_for_the_nests_setting(admin_app, nest_instance):
    """Admin → Calendar, Contacts and Files: each page's switch (and the CalDAV
    port's save) is blocked until ``get_mail_config`` answers, never flipped from
    a guessed off (admin.md, *Controls wait for their read*, Calendar / Contacts
    / Files). One journey for the three pages of the one outcome; the failure
    names the page that is short."""
    app = admin_app
    require_surface(app.driver, "painted-reason")
    pages = (
        (app.admin.navigate_calendar, ("admin-calendar-enabled-toggle",
                                       "admin-calendar-caldav-port-save-button")),
        (app.admin.navigate_contacts, ("admin-contacts-carddav-enabled-toggle",)),
        (app.admin.navigate_files, ("admin-files-webdav-enabled-toggle",)),
    )

    def blocked():
        for navigate, controls in pages:
            navigate()
            for element_id in controls:
                _assert_blocked_by_the_read(app, element_id)

    _with_nest_down(app, nest_instance, blocked)
    for navigate, controls in pages:
        _wait_read_reason_clears(app, navigate, controls[0])


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("admin-forwarders")
def test_the_add_forwarder_form_waits_for_the_forwarders_and_domains(
    admin_app, nest_instance
):
    """Admin → Aliases: the add-forwarder form is blocked until the forwarder
    and local-domain read answers, so nothing is created against a list the page
    has not shown (admin.md, *Controls wait for their read*, Aliases)."""
    app = admin_app
    require_surface(app.driver, "painted-reason")

    def blocked():
        app.admin.navigate_aliases()
        _assert_blocked_by_the_read(app, "admin-aliases-forwarder-add-submit-button")
        for field in ("admin-aliases-forwarder-add-pattern-input",
                      "admin-aliases-forwarder-add-target-input"):
            assert not app.driver.is_enabled(field), (
                f"{field} invites typing into a form that cannot be saved yet"
            )

    _with_nest_down(app, nest_instance, blocked)
    _wait_read_reason_clears(
        app, app.admin.navigate_aliases, "admin-aliases-forwarder-add-submit-button"
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("admin-bridges")
def test_the_bridge_lists_say_they_are_loading_rather_than_empty(
    admin_app, nest_instance
):
    """Admin → Bridges: the pending and approved sections say the list is
    loading, or failed to load, until the list read answers, and neither is
    painted empty on a guess (admin.md, *Controls wait for their read*,
    Bridges). The reject and rotate confirms' in-flight arm needs a write on its
    way and is the Rust machine tests' (module docstring)."""
    app = admin_app
    require_surface(app.driver, "painted-reason")
    sections = ("admin-bridges-pending-section", "admin-bridges-approved-section")

    def loading():
        app.admin.navigate_bridges_pending()
        for section in sections:
            wait_until(
                lambda section=section: app.driver.count(section) > 0,
                UI_SETTLE_S,
                diagnose=lambda section=section: f"{section} is not on the page",
            )
            reason = app.driver.painted_reason(section)
            assert reason in READ_REASONS, (
                f"{section} says {reason!r} under its heading before the list was "
                f"read, not that it is loading or failed to load ({READ_REASONS!r})"
            )
        assert app.driver.count("admin-bridges-pending-card") == 0
        assert app.driver.count("admin-bridges-approved-card") == 0

    _with_nest_down(app, nest_instance, loading)

    def read():
        app.admin.navigate_bridges_pending()
        return all(app.driver.painted_reason(s) is None for s in sections)

    wait_until(
        read,
        RPC_ROUNDTRIP_S + ONLINE_BUDGET_S,
        interval=1.0,
        diagnose=lambda: (
            "the bridge sections still say they are loading with the nest back: "
            f"{[app.driver.painted_reason(s) for s in sections]!r}. "
            f"error: {app.error_text()!r}"
        ),
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("admin-tiers")
def test_the_add_tier_form_waits_for_the_tier_list(admin_app, nest_instance):
    """Admin → Settings: the add-a-tier form is blocked until ``tiers.list``
    answers — still loading, or failed to load after a failed read, never live
    over a list the page has not shown — and a press is refused, sending nothing
    (admin.md, *Controls wait for their read*, Settings). The form's own inputs
    stay closed with it."""
    app = admin_app
    require_surface(app.driver, "painted-reason")
    require_surface(app.driver, "admin-tier-add-read-gate")

    def blocked():
        app.admin.navigate_settings()
        _assert_blocked_by_the_read(app, "admin-settings-tier-add-button")
        assert not app.driver.is_enabled("admin-settings-tier-add-name-input"), (
            "admin-settings-tier-add-name-input invites typing into a form that "
            "cannot be saved yet"
        )

    _with_nest_down(app, nest_instance, blocked)
    _wait_read_reason_clears(
        app, app.admin.navigate_settings, "admin-settings-tier-add-button"
    )

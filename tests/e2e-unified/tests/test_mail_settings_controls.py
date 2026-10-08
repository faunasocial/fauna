"""E2E journeys for what the mail-settings page SHOWS about a person's app
passwords, and what it lets them do with them.

Target state: docs/goal/ui/mail-settings.md § Layout & flow (each credential
row's name, kind, created-at and exact login with a copy button — read where
the app under test lists the rows, which on an app with the Connected apps page
is that page's roster; the add form's
generate-by-default password with the weak-password warning and strength meter
for a typed one; the serve-here switch) and docs/goal/behavior/mail-credentials.md
§ Soft revoke (retire a credential). UX/IDs: tests/e2e-unified/ui.yaml
`mail-settings` page, `mail-settings-credentials-list` and the add-credential
form.

The sibling `test_mail_credentials.py` owns the page's lifecycle (enable, add,
revoke, rotate); `test_mail_multi_credential_auth.py` owns a second password
logging in at all. These journeys read what the page shows and prove what a
revoke and the serve-here switch do to a real mail app's login — every gesture
in the app, every mail-app login over real IMAPS.

tier_3: a real app driver against a real fauna-nest; the login journeys add the
dedicated mail nest's real MTA + MDA bridges.
"""

import datetime
import time
import uuid

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.mail_dedicated_nest import ADMIN_LOCAL_PART
from helpers.mail_wire import _connect_smtp_starttls, _imap_auth_plain, _imaps_connect
from helpers.rpc_hold import (
    arm_rpc_hold,
    drop_rpc_reply_once,
    refuse_rpc,
    release_rpc_hold,
    wait_for_dropped_rpc,
    wait_for_held_rpc,
    wait_for_refused_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S

from . import test_mail_enable_then_mua_round_trip as rt

# tui leads (the lead app); the other six apps join by adding their marker once
# their run is green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.macos, pytest.mark.ios]

_UI_S = 20.0

# The wire kind every credential mint and every rotation re-wrap writes — held
# to stand inside a dispatch, refused to interrupt one
# (`libs/fauna-client-mail-settings`: `provision_mailbox_blobs`, the rotation's
# step (e)).
_WRAP_KIND = "fauna.bridges.provision_wrapped_mls_blob"
_SERVING_KIND = "fauna.bridges.set_mail_serving_enabled"

# Chosen (typed) passwords so the mail-app login uses a known value; a-zA-Z0-9
# only, as in test_mail_multi_credential_auth.py.
_DEFAULT_PASSWORD = "SettingsCtlDefaultPw01Aa"
_SECOND_PASSWORD = "SettingsCtlSecondPw02Bb"


def _wait(pred, budget_s: float = _UI_S) -> bool:
    """A deadline poll (convention 14) that answers instead of raising, so the
    caller's assert carries its own diagnosis."""
    try:
        return bool(wait_until(pred, budget_s))
    except AssertionError:
        return False


def _row_named(mail, name: str) -> int | None:
    for i in range(mail.credential_count()):
        if mail.credential_name(i) == name:
            return i
    return None


def _imap_login(handle, username: str, password: str) -> str:
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        status = _imap_auth_plain(sock, buf, "a1", username, password, deadline)
        sock.sendall(b"a9 LOGOUT\r\n")
    return status


def _enable_on_dedicated_nest(app, handle, request) -> str:
    """Log the app in as the dedicated nest's admin, turn mail on from the page
    with a typed password, bring the bridges up, and route admin@<domain> to the
    admin. Returns that address."""
    handle.assert_mta_running()
    rt._login_as_nest_admin(app, handle.nest, rt._dedicated_node_url(app, handle, request))
    mail = app.mail_settings
    mail.navigate()
    assert mail.is_page_visible(), "mail-settings page must be reachable"
    mail.enable_mail_plain(_DEFAULT_PASSWORD)
    assert mail.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"enabling mail mints the first password; error: {mail.page_error_text(timeout=2.0)!r}"
    )
    assert mail.wait_for_enabled_status(timeout=15.0), (
        f"mail must read enabled; status={mail.status_text()!r}"
    )
    handle.rebind_after_enable()
    return rt._alias_admin_to_address(handle.nest, handle.domain)


def _page_state(app) -> str:
    """The page's own account of itself, for a failure message (convention 6)."""
    mail = app.mail_settings
    d = app.driver
    return (
        f"status={mail.status_text()!r} "
        f"banner={d.is_visible('mail-settings-pending-rotation-banner')} "
        f"rotate-enabled={d.is_enabled('mail-settings-rotate-keys-button')} "
        f"error={app.error_text()!r}"
    )


# ── outcome 9 ──────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_status_line_reads_off_then_up_to_date_and_syncing_while_a_change_runs(
    dedicated_actor_app, nest_instance
):
    """Outcome 9 (off / up to date / syncing; the rotation state is the
    interrupted-rotation journey below): a person whose mail is off reads
    "Mail is disabled" — never "up to date" — and once mail is on, the line
    reads "All up to date" at rest and "Syncing" while a change is still
    running. The change is held at the nest, so "running" is a state the test
    stands in, not a race it wins."""
    app, _actor = dedicated_actor_app
    mail = app.mail_settings
    port = nest_instance["port"]
    mail.navigate()

    assert _wait(lambda: mail.status_text() == mail.STATUS_DISABLED), (
        f"mail off must read disabled; {_page_state(app)}"
    )
    assert mail.status_text() != mail.STATUS_ENABLED

    mail.enable_mail("Default")
    assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S), (
        f"mail on and settled must read up to date; {_page_state(app)}"
    )

    arm_rpc_hold(port, _WRAP_KIND)
    try:
        mail.start_add_credential("Tablet")
        wait_for_held_rpc(port, _WRAP_KIND, diagnose=lambda: _page_state(app))
        assert _wait(lambda: mail.status_text() == S.settings.mail.status_syncing), (
            f"a change still running must read syncing; {_page_state(app)}"
        )
    finally:
        release_rpc_hold(port, _WRAP_KIND)
    assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S), (
        f"the change landed: back to up to date; {_page_state(app)}"
    )


# ── outcome 11 (and outcome 9's rotation state) ────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_an_interrupted_rotation_is_shown_and_one_tap_finishes_it(
    dedicated_actor_app, nest_instance
):
    """Outcome 11: a key rotation that stops partway leaves the page saying so —
    the "didn't finish" banner, and a status line reading the rotation in
    progress (outcome 9's fourth state) — with no way to start a second
    rotation meanwhile; one tap on Resume finishes it and the page reads up to
    date again. The interruption is the nest refusing the rotation's re-wrap,
    after the rotation has already staged its new key."""
    app, _actor = dedicated_actor_app
    mail = app.mail_settings
    driver = app.driver
    port = nest_instance["port"]
    mail.navigate()
    mail.ensure_mail_enabled()
    assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S)

    refuse_rpc(port, _WRAP_KIND)
    try:
        mail.rotate_keys()
        wait_for_refused_rpc(port, _WRAP_KIND)
        assert _wait(lambda: driver.is_visible("mail-settings-pending-rotation-banner")), (
            f"an interrupted rotation must show the banner; {_page_state(app)}"
        )
    finally:
        release_rpc_hold(port, _WRAP_KIND)

    assert driver.get_text("mail-settings-pending-rotation-banner") == (
        S.settings.mail.banner_title
    )
    assert mail.status_text() == S.settings.mail.status_rotation(count="1"), (
        f"the status line must say the rotation is partway; {_page_state(app)}"
    )
    assert not driver.is_enabled("mail-settings-rotate-keys-button"), (
        f"no second rotation while one is unfinished; {_page_state(app)}"
    )

    driver.click("mail-settings-pending-rotation-resume-button")
    assert _wait(
        lambda: not driver.is_visible("mail-settings-pending-rotation-banner"),
        mail.ENABLE_SETTLE_S,
    ), f"Resume must finish the rotation and clear the banner; {_page_state(app)}"
    assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S), (
        f"a finished rotation reads up to date; {_page_state(app)}"
    )
    assert driver.is_enabled("mail-settings-rotate-keys-button"), (
        f"with the rotation finished, rotating is offered again; {_page_state(app)}"
    )


# ── outcome 12 ─────────────────────────────────────────────────────────────

# The apps whose rotate-keys form renders the per-password
# `mail-rotate-keys-exclude-item` checkboxes (and dispatches what is ticked).
_EXCLUDE_BUILT_APPS = frozenset({"tui", "macos", "ios"})


@pytest.mark.feature("turn-on-mail")
def test_a_password_marked_compromised_at_rotation_stops_working(
    app, dedicated_mail_nest, request
):
    """Outcome 12: when rotating mail keys the person can mark an app password
    they think is compromised; once the rotation finishes that password no
    longer logs a mail app in and leaves the list, while the password they
    did not mark keeps working under the new key (mail-credentials.md § Hard
    revoke → Compromised-credential handling)."""
    if app_name(app.driver) not in _EXCLUDE_BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="mail-rotate-keys-exclude-item",
            detail="marking passwords compromised at rotation is built on tui first",
            tracked="",
        )
    handle = dedicated_mail_nest
    admin_addr = _enable_on_dedicated_nest(app, handle, request)
    mail = app.mail_settings

    mail.add_credential_plain("Phone", _SECOND_PASSWORD)
    assert mail.wait_for_credential_count_at_least(2, timeout=15.0)
    phone_addr = f"{ADMIN_LOCAL_PART}+phone@{handle.domain}"
    assert _imap_login(handle, phone_addr, _SECOND_PASSWORD) == "OK", (
        "before the rotation, the Phone password logs in"
    )

    mail.rotate_keys(exclude=("Phone",))
    assert mail.wait_for_rotation_to_finish(mail.ENABLE_SETTLE_S), (
        f"the rotation must finish; {_page_state(app)}"
    )
    assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S), (
        f"a finished rotation reads up to date; {_page_state(app)}"
    )
    assert _wait(lambda: _row_named(mail, "Phone") is None), (
        f"the password marked compromised leaves the list; {_page_state(app)}"
    )

    assert _wait(lambda: _imap_login(handle, phone_addr, _SECOND_PASSWORD) != "OK", 30.0), (
        "the password marked compromised must no longer log a mail app in"
    )
    assert _imap_login(handle, admin_addr, _DEFAULT_PASSWORD) == "OK", (
        "the password not marked keeps working under the new key"
    )


# ── outcome 17 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_turning_email_off_keeps_the_calendar_signed_in(app, dedicated_mail_nest, request):
    """Outcome 17: with the calendar on, turning email off keeps the calendar
    app signed in with the same password — CalDAV still authenticates after the
    Disable, while the page reads email off. Email is turned on and off from the
    page; the calendar half is the test agent's CalDAV-mailbox command (no app
    switch turns a person's calendar on — the deployment toggle is the admin's)."""
    from helpers.mail_dedicated_nest import mint_caldav_mailbox, require_caldav_mailbox_mint_supported
    from .test_mail_multi_credential_auth import _caldav_propfind_status

    require_caldav_mailbox_mint_supported(app.driver)
    handle = dedicated_mail_nest
    admin_addr = _enable_on_dedicated_nest(app, handle, request)
    mint_caldav_mailbox(app.driver, password=None)  # calendar on, same password
    caldav = f"https://127.0.0.1:{handle.caldav_port}"
    assert _caldav_propfind_status(caldav, admin_addr, _DEFAULT_PASSWORD) in (200, 207), (
        "with email and calendar on, the calendar app signs in"
    )

    mail = app.mail_settings
    mail.disable_mail()
    assert mail.wait_for_enabled_toggle_state("off"), (
        f"email must read off after the Disable; {_page_state(app)}"
    )
    assert _caldav_propfind_status(caldav, admin_addr, _DEFAULT_PASSWORD) in (200, 207), (
        "turning email off must keep the calendar app signed in with the same password"
    )


# ── outcome 18 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_turning_your_own_mail_off_leaves_another_users_mail_working(
    dedicated_actor_app, nest_instance, mail_bridge_mda, seal_helper_binary
):
    """Outcome 18: one person turning their own mail off never turns mail off
    for anyone else on the nest — another user's mail app still logs in to
    their mailbox afterwards. The other user is arranged with their own app
    password (what their own app would have minted); the act is this person's
    Disable in the app."""
    from common.auth import create_actor_and_register
    from conftest import _provision_msek_recipient

    other_actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    other = _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_mda.domain, local_part=other_actor["handle"],
        password=f"other-{uuid.uuid4().hex[:12]}", actor=other_actor)
    assert _imap_login(mail_bridge_mda, other.username, other.password) == "OK", (
        "before anything, the other user's mail app logs in"
    )

    app, _actor = dedicated_actor_app
    mail = app.mail_settings
    mail.navigate()
    mail.ensure_mail_enabled()
    mail.disable_mail()
    assert mail.wait_for_enabled_toggle_state("off"), (
        f"this person's mail must read off; {_page_state(app)}"
    )
    assert _wait(lambda: mail.status_text() == mail.STATUS_DISABLED), _page_state(app)

    assert _imap_login(mail_bridge_mda, other.username, other.password) == "OK", (
        "turning one person's mail off must leave another user's mail app working"
    )


# ── outcome 24 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_a_refused_change_says_why_and_can_be_tried_again(
    dedicated_actor_app, nest_instance
):
    """Outcome 24: when the nest refuses a change, the page says so and leaves
    the setting as it was; trying again once the nest accepts it goes through
    and the message clears. The change is the serve-here switch."""
    app, _actor = dedicated_actor_app
    mail = app.mail_settings
    port = nest_instance["port"]
    mail.navigate()
    mail.ensure_mail_enabled()
    assert mail.wait_for_serve_here_state("on")

    refuse_rpc(port, _SERVING_KIND)
    try:
        mail.set_serve_here(False)
        wait_for_refused_rpc(port, _SERVING_KIND)
        error = mail.page_error_text(timeout=_UI_S)
        assert error, f"a refused change must say why; {_page_state(app)}"
        assert mail.serve_here_state() == "on", (
            f"a refused change leaves the setting as it was; {_page_state(app)}"
        )
    finally:
        release_rpc_hold(port, _SERVING_KIND)

    mail.set_serve_here(False)
    assert mail.wait_for_serve_here_state("off"), (
        f"trying again goes through; {_page_state(app)}"
    )
    assert _wait(lambda: not app.driver.is_visible("error-message")), (
        f"a change that went through clears the message; {_page_state(app)}"
    )
    mail.set_serve_here(True)
    assert mail.wait_for_serve_here_state("on")


# ── outcome 16 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_a_brief_drop_while_turning_mail_on_is_retried_and_finishes(
    dedicated_actor_app, nest_instance
):
    """Outcome 16: a brief connection drop while turning mail on does not undo
    it — the app retries the interrupted step and the page ends with mail on,
    the first password listed, and no error (mail-credentials.md §
    Partial-state-during-minting (first-enable): every step is idempotent, so a
    transient transport failure is retried within the one enable gesture).

    The drop is the nest running the first password's key write for real and
    then closing the connection in place of its reply — exactly once, so the
    app's own retry of the same write is answered. Nothing here is timed: the
    nest's drop counter proves the drop happened before the page is read."""
    app, _actor = dedicated_actor_app
    mail = app.mail_settings
    port = nest_instance["port"]
    mail.navigate()
    assert _wait(lambda: mail.status_text() == mail.STATUS_DISABLED), (
        f"the journey starts with mail off; {_page_state(app)}"
    )

    drop_rpc_reply_once(port, _WRAP_KIND)
    try:
        mail.enable_mail("Default")
        wait_for_dropped_rpc(port, _WRAP_KIND)
        assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S), (
            f"the retried enable must finish with mail on; {_page_state(app)}"
        )
    finally:
        release_rpc_hold(port, _WRAP_KIND)

    assert mail.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"the first password is listed after the retry; {_page_state(app)}"
    )
    assert not app.driver.is_visible("error-message"), (
        f"a drop the retry recovered from is not an error; {_page_state(app)}"
    )


# ── outcome 21 ─────────────────────────────────────────────────────────────

# The apps `conftest.py::_alice_extra_seat` can launch a second seat of one
# identity on (a fresh, empty state root, a distinct device id).
_SECOND_SEAT_APPS = frozenset({"tui", "linux", "macos", "windows"})


@pytest.mark.feature("turn-on-mail")
def test_mail_setup_and_passwords_follow_you_to_a_second_device(
    dedicated_actor_app, nest_instance, request
):
    """Outcome 21: mail turned on and an app password added on one device show
    on the person's other device without setting mail up again — the second
    device, logged in as the same identity from an empty state root, opens the
    page reading mail on, with both passwords listed under their names
    (mail-credentials.md § Goal; § MSEK lifecycle → *Persistence*: the mail
    custody is synced across every Fauna app the user owns)."""
    from conftest import _alice_extra_seat

    app, actor = dedicated_actor_app
    name = app_name(app.driver)
    if name not in _SECOND_SEAT_APPS:
        skip_unbuilt(
            app.driver,
            surface="a second seat of one identity in the e2e harness",
            detail="conftest.py::_alice_extra_seat launches tui, linux, macos and windows seats only",
            tracked="",
        )
    mail = app.mail_settings
    mail.navigate()
    mail.enable_mail("Default")
    assert mail.wait_for_enabled_status(timeout=mail.ENABLE_SETTLE_S), (
        f"mail must turn on on the first device; {_page_state(app)}"
    )
    mail.add_credential("Phone")
    assert mail.wait_for_credential_count_at_least(2, timeout=15.0), (
        f"the second password must be listed on the first device; {_page_state(app)}"
    )

    seat = _alice_extra_seat(
        name, request, actor, nest_instance, screenshot_tag="mail-second-device",
    )
    device_b, _driver_b = next(seat)
    try:
        mail_b = device_b.mail_settings
        mail_b.navigate()
        assert mail_b.wait_for_enabled_status(timeout=mail_b.ENABLE_SETTLE_S), (
            f"the second device must read mail on without setting it up; "
            f"{_page_state(device_b)}"
        )
        assert _wait(
            lambda: {_row_named(mail_b, "Default") is not None,
                     _row_named(mail_b, "Phone") is not None} == {True},
            mail_b.ENABLE_SETTLE_S,
        ), (
            f"both passwords must be listed on the second device; "
            f"{_page_state(device_b)} count={mail_b.credential_count()}"
        )
    finally:
        seat.close()


# ── outcome 10 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_credential_row_shows_name_kind_created_and_a_copyable_login(logged_in_app):
    """Outcome 10: each app password is listed with its name, its kind, when it
    was made, and the exact login a mail app needs — and the copy button puts
    exactly that login on the clipboard."""
    app = logged_in_app
    mail = app.mail_settings
    mail.navigate()
    mail.ensure_mail_enabled()

    name = f"Laptop {uuid.uuid4().hex[:4]}"
    made = datetime.datetime.now()
    mail.add_credential_plain(name, _SECOND_PASSWORD)
    assert _wait(lambda: _row_named(mail, name) is not None), (
        f"the new password's row never appeared; error: {mail.page_error_text(timeout=2.0)!r}"
    )
    i = _row_named(mail, name)

    assert mail.credential_type(i) == S.settings.mail.kind_password
    created = mail.credential_created_at(i)
    when = datetime.datetime.strptime(created, "%Y-%m-%d %H:%M")
    assert abs((when - made).total_seconds()) < 600, (
        f"the row must say when the password was made; reads {created!r}"
    )
    login = mail.credential_username(i)
    credential_id = name.lower().replace(" ", "-")
    assert login.split("@")[0].endswith(f"+{credential_id}") and "@" in login, (
        f"the login must be the exact sub-addressed mail-app username "
        f"(<handle>+{credential_id}@<domain>); reads {login!r}"
    )

    copied = mail.copy_credential_username(i)
    assert copied == login, (
        f"copy must put exactly the login on the clipboard; reads {copied!r}"
    )

    mail.revoke_credential(i)  # leave the session user's list as it was


# ── outcome 13 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_revoking_one_password_leaves_the_others_working(app, dedicated_mail_nest, request):
    """Outcome 13: revoking one app password cuts off only the mail apps using
    it — its login is refused over IMAP, while the other password still logs
    in."""
    handle = dedicated_mail_nest
    admin_addr = _enable_on_dedicated_nest(app, handle, request)
    mail = app.mail_settings

    mail.add_credential_plain("Phone", _SECOND_PASSWORD)
    assert mail.wait_for_credential_count_at_least(2, timeout=15.0)
    phone_addr = f"{ADMIN_LOCAL_PART}+phone@{handle.domain}"
    assert _imap_login(handle, phone_addr, _SECOND_PASSWORD) == "OK", (
        "before the revoke, the second password logs in"
    )

    i = _row_named(mail, "Phone")
    assert i is not None, "the Phone row must be listed"
    mail.revoke_credential(i)
    assert _wait(lambda: _row_named(mail, "Phone") is None), (
        f"a revoked password leaves the list; error: {mail.page_error_text(timeout=2.0)!r}"
    )

    assert _wait(lambda: _imap_login(handle, phone_addr, _SECOND_PASSWORD) != "OK", 30.0), (
        "the revoked password must no longer log a mail app in"
    )
    assert _imap_login(handle, admin_addr, _DEFAULT_PASSWORD) == "OK", (
        "revoking one password must leave the other one working"
    )


# ── outcome 19 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
@pytest.mark.real_conversations
def test_serving_off_closes_mail_apps_but_the_app_still_shows_mail(
    app, dedicated_mail_nest, request
):
    """Outcome 19: with serving turned off on this nest, a mail app can no
    longer open the mailbox there, while the Fauna app itself still shows the
    person's mail — a message delivered after the switch arrives in the app."""
    handle = dedicated_mail_nest
    admin_addr = _enable_on_dedicated_nest(app, handle, request)
    mail = app.mail_settings
    assert _imap_login(handle, admin_addr, _DEFAULT_PASSWORD) == "OK", (
        "with serving on (the default), a mail app logs in"
    )

    assert mail.serve_here_visible()
    mail.set_serve_here(False)
    assert mail.wait_for_serve_here_state("off"), (
        f"the serve-here switch never read off; error: {mail.page_error_text(timeout=2.0)!r}"
    )
    assert _wait(lambda: _imap_login(handle, admin_addr, _DEFAULT_PASSWORD) != "OK", 30.0), (
        "with serving off, a mail app must no longer open the mailbox on this nest"
    )

    subject = f"Still here {uuid.uuid4().hex[:8]}"
    raw = ("\r\n".join([
        "From: Someone <sender@external.test>",
        f"To: {admin_addr}",
        f"Subject: {subject}",
        f"Message-ID: <{uuid.uuid4().hex}@external.test>",
        "Date: Mon, 21 Sep 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Delivered while mail apps are closed out of this nest.",
    ]) + "\r\n").encode()
    deadline = time.monotonic() + 40.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{admin_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    def _shown():
        return any(subject in (t.label or "") for t in app.conversations.list_threads())

    assert _wait(_shown, 60.0), (
        f"the app must still show mail with serving off; threads "
        f"{[t.label for t in app.conversations.list_threads()]!r}"
    )


# ── outcome 20 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_new_password_is_generated_by_default_and_a_typed_one_is_warned(logged_in_app):
    """Outcome 20: a new app password is generated strong by default — the form
    shows it, with no warning — and a typed one gets the warning that it limits
    the mail's protection, plus a strength reading that follows what is typed."""
    mail = logged_in_app.mail_settings
    driver = logged_in_app.driver
    mail.navigate()
    mail.ensure_mail_enabled()

    driver.click("mail-settings-add-credential-button")
    driver.wait_for("mail-add-credential-type-selector", timeout=10.0)
    driver.click("mail-add-credential-type-selector")  # bearer token → password
    driver.wait_for("mail-add-credential-autogenerate-toggle", timeout=10.0)

    # The default is observable in what the form holds before anything is
    # typed: a long generated password, and no warning.
    generated = driver.get_text("mail-add-credential-password-input").strip()
    assert len(generated) >= 16, (
        f"generating the password is the default — the form shows a long one "
        f"before anything is typed; got {generated!r}"
    )
    assert driver.count("mail-add-credential-weak-password-warning") == 0, (
        "no weak-password warning while the password is generated"
    )
    assert driver.count("mail-add-credential-password-strength-meter") == 0

    driver.click("mail-add-credential-autogenerate-toggle")
    driver.clear_and_type("mail-add-credential-password-input", "abc")
    assert _wait(lambda: driver.count("mail-add-credential-weak-password-warning") == 1)
    assert driver.get_text("mail-add-credential-weak-password-warning") == (
        S.settings.mail.weak_password_warning
    )
    assert _wait(lambda: driver.get_text("mail-add-credential-password-strength-meter")
                 == S.settings.mail.strength_weak), (
        f"a short typed password reads weak; meter reads "
        f"{driver.get_text('mail-add-credential-password-strength-meter')!r}"
    )
    driver.clear_and_type("mail-add-credential-password-input", "Tr0ub4dor&3-horse-battery-staple")
    assert _wait(lambda: driver.get_text("mail-add-credential-password-strength-meter")
                 == S.settings.mail.strength_strong), (
        f"a long mixed password reads strong; meter reads "
        f"{driver.get_text('mail-add-credential-password-strength-meter')!r}"
    )

    mail.close_add_credential_form()

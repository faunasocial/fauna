"""The folder share form, the per-folder WebDAV switch and the claim redeem
offer only what can succeed: the share confirm is disabled with its reason
while no recipient is typed and the form closes and clears once the share is
issued; the WebDAV switch says it is still loading while the mail-custody read
is out; and on a nest with no payments plane the redeem block is
left out entirely, never greyed.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot, and what
this nest can never do is not shown; rule 4's form case: a form closes only
once its op is issued). The per-page instances: ``docs/goal/ui/folders.md``
§ Errors & edge cases (*The follow and share forms are painted from a
draft-validity predicate*; *Own-tier and mail gates* — ``folder-webdav-toggle``)
and ``docs/goal/ui/profile.md`` § Errors & edge cases (*Unread state is not
possible* — Redeem over the features read, a ``hidden`` payments row meaning
*absent*).

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this nest can never
serve is shown. The reason is read as painted (``PlatformDriver.blocked_reason``)
and compared with the ``S.`` string of the predicate's reason key; an app whose
driver cannot read it yet takes the ``blocked-reason-read`` surface gate.

**The share form closes once ISSUED, not once it succeeded.** ``folders.md:457``
rules that the form closes and clears its input once the share op is issued,
and stays open with what was typed only while the predicate refuses (a blank
recipient, or no live conversations session). So the journey issues a share
to a handle nobody holds: the form closes and clears, the folder stays
unshared, and whatever the failed resolution says lands on ``error-message``.

**No payments plane, without a new knob.** Every capability token the e2e
build compiles is advertised, so "a nest with no payments plane" is stood in
by the nest's test-only capability-withhold hook (``capability_withhold_test_hook``,
the same seam ``test_feature_limits.py`` uses): the nest stops advertising
``subscriptions``, so the features fold answers the ``payments`` row ``hidden``
(``fauna_client_features::feature_row``) and ``gate_possibility`` answers
*absent*. The client reads the capability set once per connection, so each
half signs in afresh on a dedicated nest (the ``fresh_nest`` shape of
``test_fresh_account_offer_only_possible.py``) — never the shared nest, whose
other tests would read the withheld set.

**Pressing a blocked control.** The disabled control IS the refusal, and the
test agent refuses a press on a disabled control (convention 11), so a journey
never presses one; the press half is the shared predicate's backstop.

Every act goes through the page (convention 8); each wait is a deadline over a
local repaint or a named read, never a settle sleep (convention 14).
"""

from __future__ import annotations

import secrets
import urllib.request

import pytest

from helpers import budgets
from helpers.app_surface import require_surface
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import await_session_actor, wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


def _diag(driver, element_id: str, scope: str | None = None) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'), scope=scope)}, "
        f"{driver.diagnose('error-message')}"
    )


def _blocked_with(driver, element_id: str, reason: str, scope: str | None) -> bool:
    if driver.is_absent(element_id, scope=scope) or driver.is_enabled(element_id, scope=scope):
        return False
    return driver.blocked_reason(element_id, scope=scope) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str,
                         scope: str | None = None) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it; the asserts after the deadline name the part that failed
    (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason, scope),
                   budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which part failed
    diag = _diag(driver, element_id, scope)
    assert not driver.is_absent(element_id, scope=scope), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id, scope=scope), (
        f"{what}: {element_id} is live while its precondition is unmet: {diag}"
    )
    painted = driver.blocked_reason(element_id, scope=scope)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _page_error(app) -> str:
    try:
        return app.error_text() or ""
    except Exception:  # noqa: BLE001 - an absent error element reads as none
        return ""


# ── share-a-folder 32 ────────────────────────────────────────────────────────


@pytest.mark.feature("share-a-folder")
@pytest.mark.real_conversations
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_share_confirm_asks_for_a_recipient_and_the_form_closes_once_the_share_is_issued(
    logged_in_app,
):
    """With the share form open and no recipient typed, the confirm is disabled
    and asks who to share with; with a recipient typed it is live; pressed, the
    share is issued and the form closes and clears — reopened, it is empty."""
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    b = app.backups

    name = f"row-6320-share-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder_until(name, "folder-share-button")
    b.open_share_dialog()

    d.clear_and_type("recipient-picker-input", "")
    _assert_blocked_with(d, "folder-share-confirm", S.folders.share_recipient_required,
                         what="a share with no recipient typed")

    d.clear_and_type("recipient-picker-input", "   ")
    _assert_blocked_with(d, "folder-share-confirm", S.folders.share_recipient_required,
                         what="a share whose recipient is only blanks")

    # A handle nobody holds: the share is issued (the predicate answers
    # possible) and fails at resolution, after the form has closed.
    recipient = f"row-6320-nobody-{secrets.token_hex(4)}"
    d.clear_and_type("recipient-picker-input", recipient)
    wait_until(
        lambda: d.is_enabled("folder-share-confirm"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the share confirm stayed disabled with a recipient typed: "
                         f"{_diag(d, 'folder-share-confirm')}",
    )
    d.click("folder-share-confirm")
    wait_until(
        lambda: not b.share_dialog_open(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the share form stayed open after the share was issued "
                         f"(folders.md § Errors & edge cases): "
                         f"{d.diagnose('recipient-picker-input')}, error={_page_error(app)!r}",
    )
    assert b.shared_member_count() == 0, (
        f"a share to a handle nobody holds must admit no member; "
        f"count={b.shared_member_count()}, error={_page_error(app)!r}"
    )

    # Reopened, the form is empty: it cleared what was typed on issue. The
    # expand toggles, and whether the row is still open once its form closed
    # differs by app (web keeps it open; a list rebuild shuts it elsewhere), so
    # re-expand only a row that no longer shows its Share button.
    if not d.is_visible("folder-share-button"):
        b.find_and_expand_folder_until(name, "folder-share-button")
    b.open_share_dialog()
    kept = d.get_text("recipient-picker-input") or ""
    assert kept.strip() == "", (
        f"the share form must clear its recipient once the share is issued; "
        f"reopened it holds {kept!r}"
    )
    _assert_blocked_with(d, "folder-share-confirm", S.folders.share_recipient_required,
                         what="the reopened share form")


# ── folders 22 (still-loading arm) ───────────────────────────────────────────


def _not_still_loading(driver, element_id: str) -> bool:
    if driver.is_absent(element_id):
        return False
    if driver.is_enabled(element_id):
        return True
    return driver.blocked_reason(element_id) != S.common.still_loading


@pytest.mark.feature("folders")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_webdav_switch_says_still_loading_while_the_mail_custody_read_is_out(
    logged_in_app,
):
    """With the mail-custody read held, a folder's WebDAV switch is shown,
    disabled, and says it is still loading — never "set up mail first" on a
    read that has not answered; released, the read answers and the switch
    stops saying so (live, or blocked on what the answer found).

    The read is the account store's, not the nest's, so ``helpers/rpc_hold``
    cannot park it: the app's own hold does (``custody-read-hold``). The rows
    paint before the read answers (tui reads it as its own op after the
    folders hydrate; web, linux and android start it after the rows and reset
    to "still loading" on every visit), so the switch is on screen while the
    read is out. The
    release half is what makes the held half honest: a switch that never
    answers would stay "still loading" and fail it.
    """
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    require_surface(d, "custody-read-hold")
    b = app.backups

    name = f"row-6370-webdav-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    # Leave the page, so the next visit reads the custody again under the hold.
    app.mail_settings.navigate()

    b.hold_custody_reads()
    try:
        b.navigate_folders()
        b.find_and_expand_folder_until(name, "folder-webdav-toggle")
        _assert_blocked_with(d, "folder-webdav-toggle", S.common.still_loading,
                             what="the WebDAV switch while the mail-custody read is out")
    finally:
        b.release_custody_reads()

    wait_until(
        lambda: _not_still_loading(d, "folder-webdav-toggle"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the WebDAV switch still says loading after the custody "
                         f"read was released: {_diag(d, 'folder-webdav-toggle')}",
    )


# ── subscriptions-and-tiers 39 ───────────────────────────────────────────────


@pytest.fixture
def no_payments_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest whose payments plane the test can withdraw."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "no-payments-nest")
    yield nest
    cleanup()


def _withhold_capability(nest: dict, token: str, withhold: bool) -> None:
    """Make the nest stop (or resume) advertising ``token`` (the test-only
    ``capability_withhold_test_hook``)."""
    suffix = "" if withhold else "/clear"
    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/capability-withhold/{token}{suffix}",
        data=b"",
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"capability-withhold returned {resp.status}"


def _sign_in_as_admin(app, nest: dict, device_id: str) -> None:
    """Point the app at ``nest`` (a relaunch, the first time this app meets
    it) and sign the admin in."""
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    admin = nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": device_id,
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    await_session_actor(app.driver, admin["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                        what="the admin sign-in")


_REDEEM_BLOCK = ("subscription-claim-redeem-input", "subscription-claim-redeem-button")


@pytest.mark.feature("subscriptions-and-tiers")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_redeem_block_is_left_out_on_a_nest_with_no_payments_plane(app, no_payments_nest):
    """On a nest that does not have the payments plane, Settings ▸
    Subscriptions shows no redeem block at all — no code field, no button, no
    error. Once the nest has the plane again, a new session shows the block:
    the absence follows the nest, not a compiled-in answer."""
    d = app.driver

    _withhold_capability(no_payments_nest, "subscriptions", True)
    try:
        _sign_in_as_admin(app, no_payments_nest, "test-device-row-6320-no-payments")
        app.subscriptions.navigate_settings()
        # Before the features read answers the redeem button is painted blocked
        # "still loading" (profile.md § Errors & edge cases), so its absence is
        # the answered paint, never a page that has not loaded.
        wait_until(
            lambda: all(d.is_absent(e) for e in _REDEEM_BLOCK),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: "a nest with no payments plane must leave the redeem "
                             "block out (rule 2: absent, never greyed): "
                             + ", ".join(_diag(d, e) for e in _REDEEM_BLOCK),
        )
        assert not _page_error(app), (
            f"leaving the redeem block out must not surface an error: {_page_error(app)!r}"
        )
    finally:
        _withhold_capability(no_payments_nest, "subscriptions", False)

    # A second sign-in to the same nest does not relaunch the app
    # (`_relaunch_trusting_nest`), and the capability set is read once per
    # connection, so the session is reset first — the `test_feature_limits.py`
    # shape — or the app would keep the withheld set it already read.
    d.reset()
    _sign_in_as_admin(app, no_payments_nest, "test-device-row-6320-payments")
    app.subscriptions.navigate_settings()
    wait_until(
        lambda: all(not d.is_absent(e) for e in _REDEEM_BLOCK),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "once the nest has the payments plane again, a new session "
                         "must show the redeem block: "
                         + ", ".join(_diag(d, e) for e in _REDEEM_BLOCK),
    )
    assert not _page_error(app), f"unexpected error: {_page_error(app)!r}"

"""While the read a control waits on is still out at the nest, or the op it
started is still in flight, the control is disabled and says so: Mark all read
and Clear all say they are still loading until the notifications page's first
read answers; the claim mint and the provider add say still loading while the
tier read is out, never "create a tier first"; another actor's Follow and
Block say still loading until their open-time reads answer; and the new
folder wizard's Create says the folder is being created while the create is
in flight.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 5: nothing
is enabled on a guess before its read; rule 2: the reason within eyeshot). The
per-page instances: ``docs/goal/behavior/notifications.md`` § Errors & edge
cases (Mark all read and Clear all blocked still loading until the page's
first read answers), ``docs/goal/ui/profile.md`` § Errors & edge cases (Follow
and Block wait on their open-time reads; claim mint and provider add say still
loading while the tier read is out) and ``docs/goal/ui/folders.md`` § Errors &
edge cases (the wizard's Create blocked ``devices.wizard.creating_folder``
while the create is in flight). These are the pending and in-flight arms of
the outcomes whose answered arms are witnessed by
``test_fresh_account_offer_only_possible.py``,
``test_folder_and_tier_forms_offer_only_possible.py`` and
``test_share_webdav_redeem_offer_only_possible.py``.

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet
or its reason is not within eyeshot. The reason is read as painted
(``PlatformDriver.blocked_reason``) and compared with the ``S.`` string of the
predicate's reason key.

**The window is stood in, not raced.** The nest holds the one wire kind the
control waits on (``helpers/rpc_hold``), armed before the app could issue it,
and ``wait_for_held_rpc`` proves the app asked before anything is read; each
test checks the hold is still parked after its assertions, so they read the
genuine pending paint. Releasing the hold lets the read answer, and the control
comes alive — the half that separates "waits for its read" from "never works".

**The gesture that issues the held read is pressed with Enter, never clicked.**
tui's automation click awaits the op a gesture starts before it acks, and its
element reads are served by that same loop, so a click whose read is held
would hold every later read with it. Enter on an element runs the same gesture
the way a person's key does — the op spawned, the page painted around it
(``automation.rs``, the ``Key`` ``Enter`` arm) — which is the window a person
stands in. The other apps paint around a pending read already; Enter on a
focused control is the same gesture there.

**The window stands until the release.** The nest announces each held read
to the app, whose dispatcher then stops that read's client deadline
(``e2e-latency-independent-assertions.md`` § The convention, *The held-read
announce*), so the pending state lasts exactly as long as the hold — however
slowly a driver reads the controls. ``_assert_still_held`` reads the hold's
``announced`` count beside ``holding``, so a park the app was never told of
fails by name instead of racing the deadline.

Every act goes through the page (convention 8); each wait is a deadline over a
local repaint or a named nest state, never a settle sleep (convention 14).
"""

from __future__ import annotations

import secrets

import pytest

from helpers import budgets
from helpers.app_surface import app_name, require_surface
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.other_profile import seed_accepted_contact
from helpers.rpc_hold import (
    arm_rpc_hold,
    release_rpc_hold,
    rpc_hold_status,
    wait_for_held_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_fresh_account_offer_only_possible import (  # noqa: F401
    _sign_in_as_admin,
    fresh_nest,
)

pytestmark = [pytest.mark.tier_3]

_NOTIFICATIONS_LIST = "fauna.notifications.list"
_TIERS_LIST = "fauna.subscriptions.tiers.list"
_CONTACTS_LIST = "fauna.contacts.list"
_SUBSCRIPTIONS_STATUS = "fauna.subscriptions.status.get"
_FOLDERS_CREATE = "fauna.folders.create"


def _diag(driver, element_id: str) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'))}, "
        f"{driver.diagnose('error-message')}"
    )


def _blocked_with(driver, element_id: str, reason: str) -> bool:
    if driver.is_absent(element_id) or driver.is_enabled(element_id):
        return False
    return driver.blocked_reason(element_id) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it; the asserts after the deadline name the part that failed
    (convention 6)."""
    _assert_all_blocked_with(driver, reason, {element_id: what})


def _assert_all_blocked_with(driver, reason: str, controls: dict[str, str]) -> None:
    """Every control in ``controls`` (id → what it is) is shown, not live, with
    the words ``reason`` painted beside it — read in ONE poll, and diagnosed
    only when the poll misses.

    One poll is the cheaper shape (a slow driver — iOS, every element read a
    round trip — pays no diagnosis on a pass); it carries no timing weight,
    because the held read is announced and its window stands until the
    test's release (the module docstring)."""
    try:
        wait_until(
            lambda: all(_blocked_with(driver, eid, reason) for eid in controls),
            budgets.UI_SETTLE_S,
        )
        return
    except AssertionError:
        pass  # the asserts below say which control, and which part, failed
    for element_id, what in controls.items():
        diag = _diag(driver, element_id)
        assert not driver.is_absent(element_id), (
            f"{what}: {element_id} must stay visible while its read is out "
            f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
        )
        assert not driver.is_enabled(element_id), (
            f"{what}: {element_id} is live before its read answered: {diag}"
        )
        painted = driver.blocked_reason(element_id)
        assert painted == reason, (
            f"{what}: the reason beside {element_id} should read {reason!r}, "
            f"painted {painted!r}: {diag}"
        )


def _assert_still_held(port: int, kind: str) -> None:
    status = rpc_hold_status(port, kind)
    assert status["holding"] >= 1, (
        f"the {kind} reply was released before the assertions above ran, so "
        f"they proved nothing about the pending window: {status!r}"
    )
    # The park was announced to the app, so its client deadline is stopped and
    # the window above stood by the hold, not by a race with that deadline.
    assert status.get("announced", 0) >= 1, (
        f"the {kind} park was never announced to the app, so its own read "
        f"deadline still bounds the pending window: {status!r}"
    )


def _comes_alive(driver, element_id: str, *, what: str) -> None:
    wait_until(
        lambda: driver.is_enabled(element_id),
        RPC_ROUNDTRIP_S + budgets.UI_SETTLE_S,
        diagnose=lambda: f"{what}: {element_id} never came alive once its read "
                         f"answered: {_diag(driver, element_id)}",
    )


# ── notifications 16 (pending arm) ───────────────────────────────────────────


@pytest.mark.feature("notifications")
@pytest.mark.tui
def test_mark_all_read_and_clear_all_say_still_loading_until_the_first_read_answers(
    app, fresh_nest,
):
    """With the page's first list read held from before sign-in, Mark all read
    and Clear all are both disabled and say they are still loading; released,
    Mark all read comes alive."""
    port = fresh_nest["port"]
    d = app.driver
    require_surface(d, "blocked-reason-read")

    arm_rpc_hold(port, _NOTIFICATIONS_LIST)
    try:
        _sign_in_as_admin(app, fresh_nest, "test-device-row-6342-notifications")
        wait_for_held_rpc(
            port, _NOTIFICATIONS_LIST,
            diagnose=lambda: f"app-side: nav={d.get_state('nav')!r}",
        )
        d.press_key("notifications-tab", "Enter")
        d.wait_for("notification-mark-read", timeout=budgets.UI_SETTLE_S)

        _assert_all_blocked_with(d, S.common.still_loading, {
            "notification-mark-read": "Mark all read before the first read answered",
            "notification-clear": "Clear all before the first read answered",
        })
        _assert_still_held(port, _NOTIFICATIONS_LIST)
    finally:
        release_rpc_hold(port, _NOTIFICATIONS_LIST)

    _comes_alive(d, "notification-mark-read", what="Mark all read")


# ── subscriptions-and-tiers 38 (pending arm) ─────────────────────────────────


@pytest.mark.feature("subscriptions-and-tiers")
@pytest.mark.tui
@pytest.mark.web
def test_claim_mint_and_provider_add_say_still_loading_while_the_tier_read_is_out(
    app, fresh_nest,
):
    """With the author's tier read held, the claim mint and the provider add
    are disabled and say still loading — never that the author owns no tier;
    released on an author with no tier, each says to create one first."""
    port = fresh_nest["port"]
    d = app.driver
    require_surface(d, "blocked-reason-read")

    arm_rpc_hold(port, _TIERS_LIST)
    try:
        _sign_in_as_admin(app, fresh_nest, "test-device-row-6342-tiers")
        subs = app.subscriptions
        subs.navigate()
        d.press_key("profile-tiers-tab", "Enter")
        wait_for_held_rpc(
            port, _TIERS_LIST,
            diagnose=lambda: f"app-side: nav={d.get_state('nav')!r}",
        )

        _assert_all_blocked_with(d, S.common.still_loading, {
            "subscription-claim-mint-button": "the claim mint while the tier read is out",
            "subscription-provider-add-button": "the provider add while the tier read is out",
        })
        _assert_still_held(port, _TIERS_LIST)
    finally:
        release_rpc_hold(port, _TIERS_LIST)

    # Released: the read answered "no tier", which is now what the two say.
    _assert_blocked_with(d, "subscription-claim-mint-button", S.subscriptions.claim_needs_tier,
                         what="the claim mint once the tier read answered no tier")
    _assert_blocked_with(d, "subscription-provider-add-button",
                         S.subscriptions.provider_needs_tier,
                         what="the provider add once the tier read answered no tier")


# ── profile 12 (Follow and Block, pending arm) ───────────────────────────────


@pytest.mark.feature("profile")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_follow_and_block_say_still_loading_until_their_open_time_reads_answer(
    logged_in_app, nest_instance, test_user,
):
    """Opening another actor's profile while its open-time reads are held —
    Block's ``contacts.list`` and Follow's ``subscriptions.status.get`` —
    Follow and Block are disabled and say they are still loading; released,
    both come alive.

    Both kinds are held because the apps order the two reads differently: tui
    issues Follow's status read behind the block read, so holding the block
    read holds both, while linux, web, macos, ios and android issue them side by side
    (an app that issued the status read first would never send the block read
    while the status read is held, and this test would read it as unsent), where the
    status read would answer at once and Follow would come alive on its own
    read. The proof that the app asked is the block read's, which every app
    issues at the open."""
    app = logged_in_app
    d = app.driver
    port = nest_instance["port"]
    require_surface(d, "blocked-reason-read")

    other = seed_accepted_contact(nest_instance, test_user)
    app.contacts.navigate()
    d.wait_for(other, timeout=budgets.UI_SETTLE_S)

    arm_rpc_hold(port, _CONTACTS_LIST)
    arm_rpc_hold(port, _SUBSCRIPTIONS_STATUS)
    try:
        # The open is the contacts page's own last step (`open_contact_profile`
        # clicks the row); the page is already shown, so the row is pressed
        # here rather than navigated to again under the hold. tui takes Enter
        # (the module docstring: its click would await the held read).
        if app_name(d) == "tui":
            d.press_key(other, "Enter")
        else:
            d.click(other)
        d.wait_for("profile-view", timeout=budgets.UI_SETTLE_S)
        wait_for_held_rpc(
            port, _CONTACTS_LIST,
            diagnose=lambda: f"app-side: nav={d.get_state('nav')!r}",
        )

        _assert_all_blocked_with(d, S.common.still_loading, {
            "profile-block-button": "Block before its open-time read answered",
            "profile-follow-button": "Follow before its open-time read answered",
        })
        _assert_still_held(port, _CONTACTS_LIST)
    finally:
        release_rpc_hold(port, _CONTACTS_LIST)
        release_rpc_hold(port, _SUBSCRIPTIONS_STATUS)

    _comes_alive(d, "profile-block-button", what="Block")
    _comes_alive(d, "profile-follow-button", what="Follow")


# ── folders 21 (in-flight arm) ───────────────────────────────────────────────


@pytest.mark.feature("folders")
@pytest.mark.tui
@pytest.mark.web
def test_the_new_folder_wizard_create_says_the_folder_is_being_created_while_in_flight(
    logged_in_app, nest_instance,
):
    """With the folder create held at the nest, the wizard's Create is disabled
    and says the folder is being created, so a second press cannot send a
    second create; released, the folder is created and its row appears."""
    app = logged_in_app
    d = app.driver
    port = nest_instance["port"]
    require_surface(d, "blocked-reason-read")
    b = app.backups

    name = f"row-6342-creating-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.add_folder()
    d.wait_for("wizard-name-input", timeout=budgets.UI_SETTLE_S)
    b.wizard_set_name(name)
    b.wizard_next()
    b.wizard_next()
    d.wait_for("wizard-create-button", timeout=budgets.UI_SETTLE_S)
    wait_until(
        lambda: d.is_enabled("wizard-create-button"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"precondition: Create never came alive on the Review step: "
                         f"{_diag(d, 'wizard-create-button')}",
    )

    arm_rpc_hold(port, _FOLDERS_CREATE)
    try:
        d.press_key("wizard-create-button", "Enter")
        wait_for_held_rpc(
            port, _FOLDERS_CREATE,
            diagnose=lambda: f"app-side: {_diag(d, 'wizard-create-button')}",
        )

        _assert_blocked_with(d, "wizard-create-button", S.devices.wizard.creating_folder,
                             what="Create while the create is in flight")
        _assert_still_held(port, _FOLDERS_CREATE)
        assert rpc_hold_status(port, _FOLDERS_CREATE)["holding"] == 1, (
            "one press sent more than one create while the first was in flight: "
            f"{rpc_hold_status(port, _FOLDERS_CREATE)!r}"
        )
    finally:
        release_rpc_hold(port, _FOLDERS_CREATE)

    b.wait_for_folder_row(name)


# ── profile 12 (private Save, the elsewhere arm) ─────────────────────────────


@pytest.mark.feature("profile")
@pytest.mark.web
def test_private_save_says_saved_elsewhere_in_a_tab_that_holds_no_account_runtime(
    logged_in_app, nest_instance, test_user,
):
    """A second tab of one account, refused the engine role, hosts no account
    runtime: on another actor's profile its private Save is disabled and says
    the notes are saved from the tab that holds it
    (``contact_overlays::save_possibility`` blocked
    ``profile.private_save_elsewhere``; ``profile.md`` § Errors & edge cases).

    web alone: a web tab that lost the MLS-writing role to another tab of the
    same account is the one seat that hosts no runtime
    (``account-client-lifecycle.md`` § The client-side lifecycle → *Bounds,
    stated*). A native app holds one runtime per process, so the arm is not
    reachable there; ``pytest.mark.web`` deselects every other app at
    collection.
    """
    from actions.contacts import ContactsActions
    from helpers.web_tabs import open_same_account_tab

    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    other = seed_accepted_contact(nest_instance, test_user)

    # Tab A takes the engine role first: its compose button is bound to the
    # manager, so ENABLED is the observable for "this tab holds the runtime"
    # (the reading `test_engine_role_election_web.py` makes).
    d.set_state({"nav": {"stack": [{"view": "conversations"}]}})
    d.wait_for("new-conversation-button", timeout=budgets.UI_SETTLE_S)
    wait_until(
        lambda: d.is_enabled("new-conversation-button"),
        60,
        diagnose=lambda: "precondition: tab A never built its conversations "
                         "manager, so the second tab could win the role: "
                         f"{d.diagnose('error-message')}",
    )

    tab_b = open_same_account_tab(d)
    try:
        ContactsActions(tab_b).open_contact_profile(other, timeout=budgets.UI_SETTLE_S)
        tab_b.wait_for("profile-view", timeout=budgets.UI_SETTLE_S)

        # The private section is painted once the tab's face has settled —
        # here on the refused role, not on a manager.
        _assert_blocked_with(
            tab_b, "profile-private-save-button", S.profile.private_save_elsewhere,
            what="private Save in the tab that holds no account runtime",
        )

        # Tab A still holds the runtime, and its Save on the same person does
        # not say "elsewhere": the refusal is the second tab's, not the
        # account's.
        assert d.is_enabled("new-conversation-button"), (
            "tab A lost its engine when tab B opened, so tab B's refusal proves "
            f"nothing about a runtime-less seat: {d.diagnose('error-message')}"
        )
        ContactsActions(d).open_contact_profile(other, timeout=budgets.UI_SETTLE_S)
        d.wait_for("profile-view", timeout=budgets.UI_SETTLE_S)
        d.wait_for("profile-private-save-button", timeout=budgets.UI_SETTLE_S)
        painted = d.blocked_reason("profile-private-save-button")
        assert painted != S.profile.private_save_elsewhere, (
            "the tab that holds the runtime also says Save is elsewhere — the "
            "refusal is not keyed on the runtime: "
            f"{_diag(d, 'profile-private-save-button')}"
        )
    finally:
        tab_b.teardown()

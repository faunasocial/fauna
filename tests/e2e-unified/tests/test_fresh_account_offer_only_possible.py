"""A fresh account is offered only what it can do: until it owns a tier the
claim mint and the payment-provider add are disabled and say to create a tier
first, with Create tier live on the same tab; and on its empty notifications
page Clear all is disabled and says there is nothing to clear, while Mark all
read stays live.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot, the
unblocking step offered beside it; rule 5: an unanswered read is never painted
as an answer). The per-page instances: ``docs/goal/ui/profile.md`` § Errors &
edge cases (*claim mint and provider add need an owned tier*) and
``docs/goal/behavior/notifications.md`` § Errors & edge cases (*Mark all read
and Clear all wait on the first read; Clear all has nothing to clear on an
empty page*).

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page that makes the precondition unmet and fails when the control is enabled
or its reason is not within eyeshot. The reason is read as painted
(``PlatformDriver.blocked_reason``) and compared with the ``S.`` string of the
predicate's reason key.

**Why a dedicated nest.** "Owns no tier" and "has no notification" are facts
about the account, and a tier can never be un-minted: on the session-shared
nest an earlier test's tier or notification would make the precondition false
(the ``paywall_nest`` shape of ``test_folder_paywall.py``). Each journey
asserts its precondition against the nest before it reads the page, so a
reorder fails by name rather than passing vacuously.

**Answered arms only.** The read-pending arms ("still loading", "failed to
load") need the page's first read held at the nest while the page is read;
tui awaits those reads on its own loop, so they are not witnessed here.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.app_surface import require_surface
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import await_session_actor, wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


@pytest.fixture
def fresh_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest: own admin, no tier, no notification."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "fresh-account-nest")
    yield nest
    cleanup()


def _admin_client(nest: dict) -> WsRpcAdminClient:
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(admin["actor_id_hex"]),
        signing_key=bytes(admin["signing_key"]),
    )


def _sign_in_as_admin(app, nest: dict, device_id: str) -> None:
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
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason), budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which part failed
    diag = _diag(driver, element_id)
    assert not driver.is_absent(element_id), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id), (
        f"{what}: {element_id} is live while its precondition is unmet: {diag}"
    )
    painted = driver.blocked_reason(element_id)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


# ── subscriptions-and-tiers 38 (answered arm) ────────────────────────────────


@pytest.mark.feature("subscriptions-and-tiers")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_claim_mint_and_provider_add_say_to_create_a_tier_until_one_is_owned(app, fresh_nest):
    """An author who owns no tier sees the claim mint and the provider add
    disabled, each saying to create a tier first, and Create tier live on the
    same tab — the one step that unblocks both."""
    _sign_in_as_admin(app, fresh_nest, "test-device-row-6221-tiers")
    d = app.driver
    require_surface(d, "blocked-reason-read")

    with _admin_client(fresh_nest) as gt:
        tiers = gt.call("fauna.subscriptions.tiers.list", {}).get("tiers", [])
    assert tiers == [], f"precondition: a fresh admin owns no tier, got {tiers!r}"

    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()

    _assert_blocked_with(d, "subscription-claim-mint-button", S.subscriptions.claim_needs_tier,
                         what="the claim mint with no tier owned")
    _assert_blocked_with(d, "subscription-provider-add-button",
                         S.subscriptions.provider_needs_tier,
                         what="the provider add with no tier owned")
    assert d.is_enabled("subscription-tier-create-button"), (
        "Create tier, the step that unblocks both, must be live on the same tab: "
        f"{_diag(d, 'subscription-tier-create-button')}"
    )


# ── notifications 16 (answered arm) ──────────────────────────────────────────


@pytest.mark.feature("notifications")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_clear_all_says_there_is_nothing_to_clear_on_an_empty_page(app, fresh_nest):
    """Once the page's first read has answered with no notification, Mark all
    read is blocked saying nothing is unread and Clear all is blocked saying
    there is nothing to clear (``notifications.md`` § Errors & edge cases, the
    page-wide actions; Mark all read's arm ruled 2026-11-19)."""
    _sign_in_as_admin(app, fresh_nest, "test-device-row-6221-notifications")
    d = app.driver
    require_surface(d, "blocked-reason-read")

    with _admin_client(fresh_nest) as gt:
        rows = gt.call("fauna.notifications.list", {}).get("notifications", [])
    assert rows == [], f"precondition: a fresh admin holds no notification, got {rows!r}"

    app.notifications.navigate()
    # The first read has answered once Mark all read paints its answered reason:
    # before it, both actions are blocked "still loading" (the same shared read
    # state); after it, with nothing unread, Mark all read reads "nothing unread".
    _assert_blocked_with(d, "notification-mark-read", S.notifications.nothing_unread,
                         what="Mark all read with nothing unread")
    assert d.count("notification-item") == 0, (
        f"precondition: the page shows no notification, shows {d.count('notification-item')}"
    )
    _assert_blocked_with(d, "notification-clear", S.notifications.nothing_to_clear,
                         what="Clear all on an empty page")

"""tier_3 e2e: the Settings → Encryption page, read and driven through the app.

The ``[app]`` outcomes of ``docs/features/encryption-settings.md`` the page
itself carries:

* **2** — Settings has an Encryption page showing how many starter keys are
  waiting (the count the page states is the nest's own count);
* **3** / **8** — the page calls the supply low when FEWER THAN FIVE are left:
  five is healthy, four is low (the shared ``fauna_conversations::keypackages_low``);
* **9** — opening the page only shows the count: the supply is unchanged by a
  visit, however low it is;
* **4** — Refresh tops the supply up on demand and the page shows the new count.

and the two the app's sign-in carries: **1** (every sign-in tops the supply
up, so a peer can start a conversation while the account is away) and **11**
(a sign-in as a new device publishes that device's keys).

Goal doc: ``docs/goal/behavior/direct-messages.md`` § Key Package Management
(the replenish model: a read-only low flag, never an auto-mint trigger); ids
``encryption-keypackage-*`` user-approved 2026-09-25 under rule A, reached
through ``settings-nav-row[encryption]`` (``docs/goal/ui/settings.md`` §
Encryption page).

A DEDICATED actor (``dedicated_actor_app``): the journey empties the account's
supply, which on the shared session user would strand every later
conversation test that needs it reachable.

The supply is moved the way the world moves it — the app never does it on its
own: the ``test-hooks`` endpoint ``POST /api/v1/test/keypackages/age`` expires
every key the account holds (the thirty-day line, ``test_starter_key_supply.py``),
and a precise remainder is set by publishing real key packages minted for the
account's identity over the nest's own upload kind. Every user gesture — opening
the page, Refresh — goes through the app (convention 8). Latency-independent
(convention 14): each assertion waits on state, never a clock.

tui is the lead app and windows the second; the other five carry the ids in
their trickle-down rows and join then. windows runs the two sign-in journeys
in full; the three that open the page take a ``skip_unbuilt`` at the rail
click until its Settings rail rows carry ``settings-nav-row[<key>]``.

``real_conversations``: the sign-in top-up (outcomes 1, 10 and 11, and every
journey's opening ``_wait_signed_in_supply``) is published by the shared
``ConversationsSession::start_receive_loop``. A native app's e2e login starts
that loop only under the marker's launch-time gate — without it windows keeps
its mock conversations backend and the nest's count stays at 0 — while a real
sign-in always starts it. The gate is session-wide on the native apps, so run
this file in its own pytest invocation there (``pytest.ini``).
"""

from __future__ import annotations

import json
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until
from tests.api.conv_api import mint_key_packages

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.windows,
    pytest.mark.real_conversations,
]

COUNT = "fauna.conversations.keypackage.count"
UPLOAD = "fauna.conversations.keypackage.upload"
#: `KEYPACKAGE_TARGET` (`libs/fauna-conversations/src/session.rs`).
TARGET = 20
#: Past the thirty-day expiry (`direct-messages.md` § Key Package Management).
EXPIRED_SECS = 31 * 24 * 3600

ROW = "settings-nav-row[encryption]"
COUNT_ID = "encryption-keypackage-count"
LOW_ID = "encryption-keypackage-low-warning"
REFRESH_ID = "encryption-keypackage-refresh-button"
BUDGET_S = 60.0


# ── the nest side: read and move the supply ──────────────────────────────────


def _client(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


def _nest_count(nest, actor) -> int:
    with _client(nest, actor) as owner:
        return owner.call(COUNT, {"actor_id": actor["actor_id_hex"]})["count"]


def _expire_all(nest, actor) -> None:
    body = json.dumps({"actor_id": actor["actor_id_hex"], "secs": EXPIRED_SECS}).encode()
    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/keypackages/age",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"key-package aging hook returned {resp.status}"
    assert _nest_count(nest, actor) == 0, "every key expired, so none may count"


def _leave_exactly(nest, actor, n: int) -> None:
    """Expire the supply, then publish exactly ``n`` real keys for ``actor``."""
    _expire_all(nest, actor)
    packages = [bytes(p) for p in mint_key_packages(bytes(actor["signing_key"]), n)]
    with _client(nest, actor) as owner:
        stored = owner.call(UPLOAD, {"packages": packages, "last_resort": False})["stored"]
    assert stored == n, f"published {stored} of {n} keys"
    assert _nest_count(nest, actor) == n


# ── the app side: the page, by clicks ────────────────────────────────────────


def _open_page(driver) -> None:
    """Reach Settings → Encryption the way a human does, from wherever the app
    is: leave the page if it is open (so this visit is a fresh one), then
    ``settings-tab`` and the rail row."""
    if driver.is_windows():
        skip_unbuilt(
            driver,
            surface=ROW,
            detail="windows paints the Encryption page, but its Settings rail rows carry no "
            "settings-nav-row[<key>] ids yet, so no click reaches it",
            tracked="ui.yaml navigation.sub_page_nav_rows",
        )
    if driver.is_visible(COUNT_ID):
        driver.click("settings-nav-back")
    if not driver.is_visible(ROW):
        driver.click("settings-tab")
        driver.wait_for(ROW, timeout=BUDGET_S)
    driver.click(ROW)
    driver.wait_for(COUNT_ID, timeout=BUDGET_S)


def _page_count(driver) -> int:
    """The count the page states, once it has read one (it shows the loading
    copy before)."""
    text = wait_until(
        lambda: (t := driver.get_text(COUNT_ID).strip()).isdigit() and t,
        BUDGET_S,
        diagnose=lambda: driver.diagnose(COUNT_ID),
    )
    return int(text)


def _wait_signed_in_supply(nest, actor) -> None:
    """Sign-in tops the supply up (outcome 1's mechanism); wait for it to land
    so it cannot race the journey's own moves."""
    wait_until(
        lambda: _nest_count(nest, actor) >= TARGET,
        BUDGET_S,
        diagnose=lambda: f"nest count {_nest_count(nest, actor)}, want >= {TARGET}",
    )


# ── the journeys ─────────────────────────────────────────────────────────────


@pytest.mark.feature("encryption-settings")
def test_the_page_shows_the_nests_count_and_calls_it_low_below_five(
    dedicated_actor_app, nest_instance
):
    """encryption-settings 2, 3, 8: the page states the nest's own count; five
    left is not low, four is — and the warning names nothing before a read."""
    app, actor = dedicated_actor_app
    driver = app.driver
    _wait_signed_in_supply(nest_instance, actor)

    _open_page(driver)
    assert _page_count(driver) == _nest_count(nest_instance, actor), (
        "the page must state the count the nest holds"
    )
    assert driver.is_absent(LOW_ID), "a full supply is not low"

    _leave_exactly(nest_instance, actor, 5)
    _open_page(driver)
    assert _page_count(driver) == 5
    assert driver.is_absent(LOW_ID), "five left is NOT low — low is fewer than five"

    _leave_exactly(nest_instance, actor, 4)
    _open_page(driver)
    assert _page_count(driver) == 4
    driver.wait_for(LOW_ID, timeout=BUDGET_S)
    assert "4" in driver.get_text(LOW_ID), (
        f"the warning names the count left: {driver.get_text(LOW_ID)!r}"
    )


@pytest.mark.feature("encryption-settings")
def test_opening_the_page_never_tops_up_and_refresh_does(dedicated_actor_app, nest_instance):
    """encryption-settings 9 and 4: an empty supply stays empty across two
    visits to the page — opening it only reads — and Refresh then tops it up to
    the target, the page showing the new count."""
    app, actor = dedicated_actor_app
    driver = app.driver
    _wait_signed_in_supply(nest_instance, actor)
    _expire_all(nest_instance, actor)

    _open_page(driver)
    assert _page_count(driver) == 0
    driver.wait_for(LOW_ID, timeout=BUDGET_S)
    _open_page(driver)  # a second, fresh visit reads whatever the first left
    assert _page_count(driver) == 0, "opening the page must not publish keys"
    assert _nest_count(nest_instance, actor) == 0, "opening the page must not publish keys"

    driver.click(REFRESH_ID)
    wait_until(
        lambda: _page_count(driver) >= TARGET,
        BUDGET_S,
        diagnose=lambda: driver.diagnose(COUNT_ID),
    )
    assert _nest_count(nest_instance, actor) == _page_count(driver), (
        "after Refresh the page states the nest's new count"
    )
    assert driver.is_absent(LOW_ID), "a topped-up supply is not low"


# ── sign-in publishes: outcomes 1 and 11 ─────────────────────────────────────


def _sign_in(app, request, nest, user, *, device_id=None) -> None:
    """A sign-in as ``user`` through the app's own session start — the
    dedicated fixture's login path, with the live-actor barrier on so a
    silently-declined switch can never pass for a sign-in."""
    from conftest import _login_app_as

    _login_app_as(app, request, nest, user, verify_live_actor=True, device_id=device_id)


@pytest.mark.feature("encryption-settings")
def test_each_sign_in_tops_up_the_supply_so_a_peer_can_start_while_you_are_away(
    dedicated_actor_app, nest_instance, request
):
    """encryption-settings 1: the supply is topped up at EVERY sign-in, not only
    the first, and the keys it leaves are what lets someone start a
    conversation with you while you are not there.

    The account's keys are all expired; the app signs in as someone else (the
    account is away) and back again — that second sign-in brings the supply
    back to the target. Then, with the app signed in elsewhere once more, a
    peer takes a starter key: the nest hands out a ONE-USE key (the supply
    drops by exactly one), so the conversation can begin with nobody online
    on this account. The count drop is what tells a one-use key from the
    reusable last-resort key, which a fetch also returns (outcome 6)."""
    from conftest import _make_user

    app, actor = dedicated_actor_app
    _wait_signed_in_supply(nest_instance, actor)
    other = _make_user(nest_instance)

    _sign_in(app, request, nest_instance, other)
    _expire_all(nest_instance, actor)
    _sign_in(app, request, nest_instance, actor)
    _wait_signed_in_supply(nest_instance, actor)

    _sign_in(app, request, nest_instance, other)  # the account is away again
    before = _nest_count(nest_instance, actor)
    peer = _make_user(nest_instance)
    with _client(nest_instance, peer) as starter:
        reply = starter.call(
            "fauna.conversations.keypackage.fetch", {"actor_id": actor["actor_id_hex"]}
        )
    assert reply.get("key_package"), f"a peer must be handed a starter key: {reply!r}"
    after = _nest_count(nest_instance, actor)
    assert after == before - 1, (
        "the key a peer starts with is one of the one-use keys the sign-in left, "
        f"not the last-resort key (count {before} → {after})"
    )


@pytest.mark.feature("encryption-settings")
def test_signing_in_on_another_device_publishes_fresh_keys_from_it(
    dedicated_actor_app, nest_instance, request
):
    """encryption-settings 11 (`devices.md` § User Experience): with every key
    the account held expired, signing the account in as a NEW device brings
    the supply back to the target — the keys are that device's own, since
    nothing else of the account is signed in to publish any (the app first
    signs in as someone else, so the old device's session is gone)."""
    from conftest import _make_user

    app, actor = dedicated_actor_app
    _wait_signed_in_supply(nest_instance, actor)

    _sign_in(app, request, nest_instance, _make_user(nest_instance))
    _expire_all(nest_instance, actor)
    _sign_in(app, request, nest_instance, actor, device_id="encryption-second-device")
    _wait_signed_in_supply(nest_instance, actor)


# ── encryption-settings 10: a new identity is reachable once it is set up ────


@pytest.fixture
def unclaimed_nest(request, nest_mode, tmp_path_factory):
    """A fresh UNCLAIMED nest on loopback with self-signed TLS — the box a new
    identity claims — the `test_onboarding_logged_in_terminal_empty_store.py`
    posture."""
    from conftest import MAIL_PRIMARY_DOMAIN, _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "new-identity-supply-nest",
        handle_domain_seed=MAIL_PRIMARY_DOMAIN, unclaimed=True, serve_tls=True,
    )
    assert nest["admin"] is None, "the fixture hands the wizard a genuinely unclaimed nest"
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.feature("encryption-settings")
def test_a_new_identity_is_reachable_as_soon_as_it_is_set_up(app, unclaimed_nest):
    """encryption-settings 10 (`direct-messages.md` § Key Package Management):
    the app publishes a new identity's first starter keys when it is set up, so
    someone can start a conversation with it straight away.

    Every step is the user's: Create Identity in the wizard, the handle, the
    claim code, the network choice; then Settings → Encryption, whose count is
    the nest's own (outcome 2). No other session ever held this identity, so
    every key the nest counts for it was published by this app's set-up."""
    from common.nest import CLAIM_CODE
    from conftest import _relaunch_trusting_nest
    from helpers.authenticated_shell import SHELL_MARKERS

    driver = app.driver
    ob = app.onboarding
    _relaunch_trusting_nest(driver, unclaimed_nest)
    ob.navigate_to_status()
    ob.generate_identity()
    driver.click("identity-continue-button")
    # identity_created → recovery_kit → handle_entry where the app shows the
    # kit page (`test_onboarding.py::test_confirm_identity_commits_through_the_shared_registry`).
    wait_until(
        lambda: driver.is_visible("recovery-kit-confirm-button") or driver.is_visible("handle-input"),
        BUDGET_S,
        diagnose=lambda: f"error={app.error_text()!r} {driver.diagnose('handle-input')}",
    )
    if driver.is_visible("recovery-kit-confirm-button"):
        ob.confirm_recovery_kit()
    driver.wait_for("handle-input", timeout=BUDGET_S)

    ob.fill_handle(f"admin@127.0.0.1:{unclaimed_nest['port']}")
    ob.run_handle_check(timeout=45)
    ob.submit_handle()
    driver.wait_for("claim-code-input", timeout=BUDGET_S)
    driver.clear_and_type("claim-code-input", CLAIM_CODE)
    driver.click("claim-code-submit-button")

    def landed() -> bool:
        if any(driver.is_visible(m) for m in SHELL_MARKERS):
            return True
        try:
            ob.finish_nat_mode()
        except Exception:  # noqa: BLE001 — the choice may not be showing yet
            pass
        return False

    wait_until(
        landed,
        120.0,
        interval=2.0,
        diagnose=lambda: f"error={app.error_text()!r} {driver.diagnose('feed-view')}",
    )

    def stated() -> int:
        _open_page(driver)
        return _page_count(driver)

    count = wait_until(
        lambda: (n := stated()) > 0 and n,
        BUDGET_S,
        diagnose=lambda: f"{driver.diagnose(COUNT_ID)} error={app.error_text()!r}",
    )
    assert count > 0, "the new identity holds starter keys a peer can fetch"

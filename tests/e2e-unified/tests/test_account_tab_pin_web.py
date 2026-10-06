"""tier_3 e2e: web's concurrent-instances leg — two tabs, two accounts.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*Web*. The browser is already a concurrent-instance host: tabs share one origin
store. Before per-tab pinning, every tab resolved its identity from the
**global** ``active`` pointer in the registry on every page load — so a switch in one tab silently re-pointed every *other* tab at the newly
activated account. This file is that hazard's witness, and the pin's.

**Why the second page must share a BrowserContext.** ``open_twin_page()`` gives
a page its own context (fresh ``localStorage``/``IndexedDB``) — a second
*device*. Two tabs of one browser profile share the store, and that sharing is
the entire subject: with nothing shared there is nothing for a pin to isolate,
so a twin-based version of this test would pass against no mechanism at all.
``open_same_context_tab()`` is the shape that can fail (``drivers/web.py``).

**Why the assertion is polarised admin-side.** The two seeded accounts differ in
a way the UI renders per tab: ``admin-tab`` is present only for a live,
authenticated *admin* session. Asserting its PRESENCE in the tab that stayed
pinned to the admin account is therefore a positive signal — it cannot be
satisfied by a tab that merely failed to authenticate, which an "absent"
assertion could. The regression this guards reads as ``admin-tab`` vanishing
from tab A after tab B switched away, with no error anywhere: the silent
convergence the goal doc names.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); web app only.
"""
from __future__ import annotations

import time

import pytest

from drivers import create_driver
from tests.test_account_switcher_web import (
    ACCOUNT_PAGE_NAV,
    ACTIVE_INDICATOR,
    ADMIN_TAB,
    ITEM_HANDLE,
    SWITCHER_ITEM,
    SWITCHER_LIST,
    _auth_on_account_page,
    _seed_two_accounts,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

PIN_KEY = "fauna_tab_account"


def _eval_settled(driver, js, *, budget_s=60, what="the read"):
    """``eval_js``, retried through a navigation.

    A switch ends in ``window.location.assign``, and the pin this test waits on
    is written *before* that call — so "the pin changed" does NOT mean the page
    has finished reloading, and any read taken just after it can land in the
    window where the execution context is being replaced. The bridge reports
    that honestly as a 500, "Execution context was destroyed, most likely
    because of a navigation"; it is the middle of the transition, not a failure.

    Retrying is sound for both slots this test reads rather than merely
    convenient: ``localStorage`` is per-origin and ``sessionStorage`` per-tab, so
    each holds the same value on either side of the reload. Only the context
    goes away, never the answer.
    """
    deadline = time.monotonic() + budget_s
    last_err = None
    while time.monotonic() < deadline:
        try:
            return driver.eval_js(js)
        except RuntimeError as e:
            # Narrow on purpose — any other bridge RuntimeError still propagates.
            if "Execution context was destroyed" not in str(e):
                raise
            last_err = e
            time.sleep(0.5)
    raise AssertionError(f"{what}: page never settled within {budget_s}s; last error {last_err}")


def _pin(driver):
    """This tab's account pin — ``sessionStorage``, so per-tab by construction."""
    return _eval_settled(driver, f"sessionStorage.getItem({PIN_KEY!r})", what="reading the pin")


def _active_secret(driver):
    """The origin-SHARED active account's secret (the registry's `fauna/index`
    active pointer + its per-actor secret row). Both tabs read the same value
    here; that is precisely why a pinned tab must not resolve its identity
    from the active pointer."""
    return _eval_settled(
        driver,
        "(() => { const idx = JSON.parse(localStorage.getItem('fauna/index') || 'null');"
        " return idx && idx.active ? localStorage.getItem('fauna/' + idx.active + '/secret') : null; })()",
        what="reading the active account's secret",
    )


def _await_pin(driver, expected, *, budget_s=60, what="the pin"):
    """Wait until this tab's pin is ``expected`` — a deadline poll on
    latency-independent state, not a settle-sleep (e2e-conventions.md point 14).

    A switch ends in ``window.location.assign``, so the pin is written and then
    the page is torn down and rebuilt; the switcher list is visible on BOTH sides
    of that navigation, so waiting on the list does not mean the switch has
    landed. Reading the pin straight after the click therefore samples the
    outgoing page — which is exactly how this assertion first failed, with the
    error message's own re-read already showing the correct value.
    """
    deadline = time.monotonic() + budget_s
    last = None
    while time.monotonic() < deadline:
        last = _pin(driver)  # already navigation-tolerant — see `_eval_settled`
        if last == expected:
            return
        time.sleep(0.5)
    raise AssertionError(
        f"{what}: expected {expected!r} within {budget_s}s, last saw {last!r}"
    )


# Its outcome — two windows holding two identities at once — is owned by
# `docs/features/second-identity-in-its-own-window.md` (outcome 2), not by
# `multiple-accounts`; putting it on the latter would state the same promise
# on two pages, which the catalog's one-owner rule forbids.
@pytest.mark.feature("second-identity-in-its-own-window")
def test_web_second_tab_switch_does_not_drag_the_first_tab_along(nest_instance, spa_url):
    """Tab B switching accounts must leave tab A serving the account it had.

    The pre-pin behaviour: tab A's next boot resolves its identity from the
    global ``active`` pointer — now tab B's choice — so tab A becomes tab B's
    account. Nothing errors; the user simply finds a different identity in a tab
    they never touched.
    """
    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        # (1) Tab A signs in as the regular user, then SWITCHES to the admin —
        # the proven path of `test_account_switcher_web.py`'s own switch journey,
        # reused deliberately. `admin-tab` is only revealed by a live,
        # authenticated admin session (`checkIsAdmin` is a real nest call), so an
        # in-memory `set_state` patch cannot produce it and must not be used to
        # fake this precondition; the switch's own reload + silent sign-in is
        # what makes the tab genuinely the admin.
        _auth_on_account_page(tab_a, seed, user_secret, "user", user_actor, spa_url)
        tab_a.click(SWITCHER_ITEM, index=1)
        tab_a.wait_for(ADMIN_TAB, timeout=60)
        assert tab_a.is_visible(ADMIN_TAB), "tab A is now genuinely the admin account"

        # Switching pins the tab that switched. That pin IS the isolation:
        # without it the boot re-points this tab at the last-activated
        # account on its very next mount.
        _await_pin(tab_a, admin_actor, what="the switching tab pins itself to its choice")

        # (2) A second TAB of the same browser profile — one shared origin store,
        # its own sessionStorage (so its own, initially absent, pin).
        tab_b = tab_a.open_same_context_tab()
        tab_b.set_state(ACCOUNT_PAGE_NAV)
        tab_b.wait_for(SWITCHER_LIST, timeout=60)
        tab_b.wait_for(SWITCHER_ITEM, timeout=30)

        # (3) Tab B switches to the regular user: `set_active` moves the global
        # pointer (which the origin-shared registry holds) and tab B pins
        # itself. Both are correct and none of them is tab A's business.
        tab_b.click(SWITCHER_ITEM, index=0)
        # Deliberately NOT `wait_for(SWITCHER_LIST)` here. The switch reloads the
        # page, and a reload discards the injected nav that put this tab on the
        # Account sub-page — so after it lands, the switcher list is legitimately
        # gone. Waiting on it therefore succeeds only by matching the OUTGOING
        # page and fails outright whenever the reload wins the race: it passed
        # once and timed out on the next run, from the same code. `_await_pin` is
        # the honest terminal condition — it is the state being waited for, and
        # it tolerates the navigation in between.
        _await_pin(tab_b, user_actor, what="the switching tab pins itself to its choice")
        assert _active_secret(tab_b) == user_secret, (
            "the switch must still move the registry's active pointer to the "
            "newly-active account — pinning does not change the global pointer"
        )

        # (4) The witness. Tab A reloads with the shared registry's active
        # pointer now naming the USER (so the active secret is the user's) —
        # and must still come back as the admin, because its pin, not the
        # shared pointer, is what resolves its session.
        assert _active_secret(tab_a) == user_secret, (
            "precondition: tab A's origin-shared active pointer really did change "
            "under it — otherwise this test proves nothing"
        )
        tab_a.hard_reload()
        tab_a.wait_for(ADMIN_TAB, timeout=60)
        assert tab_a.is_visible(ADMIN_TAB), (
            "tab A must still serve the admin account after tab B switched away: "
            "a pinned tab resolves its identity through the registry for ITS "
            "account, never through the origin-shared active pointer"
        )
        assert _pin(tab_a) == admin_actor, "tab A's pin survives a sibling's switch"
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()


def _await_session_actor(driver, expected, *, budget_s=60, what="the session"):
    """Wait until this tab's published ``session.actor_id`` is ``expected`` — a
    deadline poll on state, never a settle-sleep (convention 14). A switch
    reloads the page, so a read can land mid-navigation; the bridge then errors
    or reports the outgoing page, and the poll simply reads again.

    ``session.authenticated`` is deliberately NOT part of the condition: it is
    ``identity.registered``, which only the silent challenge's completion sets
    (`$lib/store`), so a seat arranged through the session patch publishes no
    such key at all (measured 2026-09-20 — the state read back carried every
    other session field and no ``authenticated``). What proves a LIVE session
    here is `admin-tab`, which only a real `am-i-admin` call reveals."""
    deadline = time.monotonic() + budget_s
    last = None
    while time.monotonic() < deadline:
        try:
            last = (driver.get_state() or {}).get("session", {})
        except RuntimeError:
            last = None
        if last and last.get("actor_id") == expected:
            return
        time.sleep(0.5)
    raise AssertionError(
        f"{what}: expected actor {expected!r} within {budget_s}s, last saw {last!r}"
    )


# NOT a catalog witness either: the second half of the same boot regression, at
# the one entry point that has no route of its own.
#
# The root layout owns the `/app` redirect, and `routes/+page.svelte` is empty
# *because* it owns it. When a PINNED tab's material cannot be read yet (the
# registry read needs wasm, whose init is async), the guard deliberately defers
# rather than answering "unauthenticated" — but a bare return parked the tab:
# on the bare index no route runs `identity.init()`, so nothing redirected it
# and nothing resolved its identity. Measured 2026-09-20 — a pinned tab sat with
# `actor_id: null`, its pin never walked, its own registry entry readable beside
# it. A tab a user cannot get out of by reloading is a
# client-unrecoverable state (`nest/common.md` § Client-state recoverability).
def test_web_a_pinned_tab_entering_at_the_app_root_is_not_parked(nest_instance, spa_url):
    """A pinned tab that enters at `/app/` resolves its account and lands.

    The pin is set before the load, so the guard meets exactly the case it
    defers on: a pinned tab whose material it cannot read yet.
    """
    seed, user_actor, user_secret, _admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        _auth_on_account_page(tab_a, seed, user_secret, "user", user_actor, spa_url)

        tab_b = tab_a.open_same_context_tab()
        tab_b.eval_js(f"sessionStorage.setItem({PIN_KEY!r}, {user_actor!r})")
        tab_b.hard_reload()

        _await_session_actor(
            tab_b,
            user_actor,
            what="the pinned tab resolves its account instead of parking at /app/",
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()


# NOT a catalog witness: a regression test for web's BOOT, and the only test
# here whose tab resolves its identity from the origin store alone.
#
# `$lib/store`'s `loadIdentity` derives the actor id through a wasm face, and
# every app route calls `identity.init()` BEFORE its own `await ensureWasm()`
# (deliberately — the actor-scope seam must be live before the module
# instantiates). So a tab with a registry active account and no pin used to THROW there, and
# the throw escaped the route's `onMount`: no identity, no `accountsBoot()`, and
# nothing that re-runs either for that load. Measured 2026-09-20 — such a tab
# published `actor_id: null` for 60 s with its own secret readable beside it —
# and a second tab of a signed-in profile is exactly that tab, since a new tab's
# `sessionStorage` (hence its pin) starts empty.
def test_web_a_second_tab_boots_into_the_signed_in_account(nest_instance, spa_url):
    """A second tab of a signed-in profile comes up on that account by itself.

    No session patch and no pin for the new tab: what is under test is what a
    tab resolves at boot from the store every tab of the profile shares.
    """
    seed, user_actor, user_secret, _admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        # Tab A signs in; the helper seeds the registry into
        # localStorage, which is what the second tab will have to read.
        _auth_on_account_page(tab_a, seed, user_secret, "user", user_actor, spa_url)

        tab_b = tab_a.open_same_context_tab()
        _await_session_actor(
            tab_b, user_actor, what="the second tab resolves the account at boot"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()


# Outcome 1 of `docs/features/second-identity-in-its-own-window.md` — the
# OPENING, where the test above witnesses outcome 2's coexistence. On web the
# place of its own is a tab and the door is that tab's own switcher
# (`account-scoping.md` § Concurrent instances → *Web*: per-tab account choosing
# rides the same switcher affordance, because tabs are web's instance story).
@pytest.mark.feature("second-identity-in-its-own-window")
def test_web_new_tab_switcher_opens_the_other_identity_in_that_tab(nest_instance, spa_url):
    """From a new tab's switcher, the other identity opens in that tab while the
    first tab keeps the identity it had — a second identity in a place of its
    own, reached through the switcher."""
    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        # Tab A serves the regular user.
        _auth_on_account_page(tab_a, seed, user_secret, "user", user_actor, spa_url)
        _await_session_actor(tab_a, user_actor, what="tab A serves the user")

        # A new tab of the same profile, on the Account page's switcher. With the
        # user still active, the admin row is index 1 — the same row the switch
        # journey in `test_account_switcher_web.py` picks.
        tab_b = tab_a.open_same_context_tab()
        tab_b.set_state(ACCOUNT_PAGE_NAV)
        tab_b.wait_for(SWITCHER_LIST, timeout=60)
        tab_b.wait_for(SWITCHER_ITEM, timeout=30)

        # Its switcher opens the admin identity there.
        tab_b.click(SWITCHER_ITEM, index=1)
        _await_pin(tab_b, admin_actor, what="the new tab pins itself to the identity it opened")
        _await_session_actor(tab_b, admin_actor, what="the new tab serves the admin identity")
        tab_b.wait_for(ADMIN_TAB, timeout=60)
        assert tab_b.is_visible(ADMIN_TAB), (
            "the new tab must be genuinely signed in as the admin identity it "
            "opened — `admin-tab` renders only for a live admin session"
        )

        # The first tab still serves the identity it had: the second identity
        # opened in a place of its own rather than taking this one over.
        _await_session_actor(tab_a, user_actor, what="tab A still serves the user")
        assert tab_a.is_absent(ADMIN_TAB), (
            "tab A turned into the admin identity when the new tab opened it — "
            "the second identity did not get a place of its own"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()


# Outcome 3 of `docs/features/second-identity-in-its-own-window.md` — what
# "launching the app again while it is open" MEANS on web, ratified 2026-09-26
# (`account-scoping.md` § Concurrent instances → *Web*, the second-launch
# paragraph): a new tab of the profile is the second launch; it comes back up
# on the identity already running rather than parking or asking, pins itself
# to it, and its switcher offers the other identities. Web does not raise the
# tab that is already open — a page has no handle on its sibling tabs — and the
# ruling does not ask it to. The boot regression test above shares the first
# step and is deliberately NOT this witness: it proves the tab resolves at all;
# this proves what the launch promises.
@pytest.mark.feature("second-identity-in-its-own-window")
def test_web_launching_again_comes_up_on_the_running_identity_and_offers_the_others(
    nest_instance, spa_url
):
    """A second tab comes up on the identity already running, pins itself to it,
    and its switcher lists the other identity beside it."""
    seed, user_actor, user_secret, _admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        _auth_on_account_page(tab_a, seed, user_secret, "user", user_actor, spa_url)
        _await_session_actor(tab_a, user_actor, what="tab A serves the user")

        # The second launch: a new tab of the same profile — no pin, no session
        # patch, exactly what a user opening the app again gets.
        tab_b = tab_a.open_same_context_tab()
        _await_session_actor(
            tab_b, user_actor, what="the new tab comes up on the identity already running"
        )
        _await_pin(tab_b, user_actor, what="the new tab pins itself to the identity it came up on")
        assert tab_b.is_absent(ADMIN_TAB), (
            "the new tab came up as the admin identity, not the one already running"
        )

        # ... and offers the other identities: its switcher lists both seeded
        # accounts, exactly one of them marked active, the admin's handle among
        # them — the row a user picks to open the other identity in this tab.
        tab_b.set_state(ACCOUNT_PAGE_NAV)
        tab_b.wait_for(SWITCHER_LIST, timeout=60)
        tab_b.wait_for(SWITCHER_ITEM, timeout=30)
        assert tab_b.count(SWITCHER_ITEM) == 2, (
            f"the new tab's switcher must offer both identities, "
            f"listed {tab_b.count(SWITCHER_ITEM)}"
        )
        assert tab_b.count(ACTIVE_INDICATOR) == 1, (
            "exactly one row — the identity already running — is marked active"
        )
        handles = tab_b.get_texts(ITEM_HANDLE)
        assert any("admin" in h for h in handles), (
            f"the other identity must be offered in the new tab's switcher; rows: {handles!r}"
        )

        # The first tab is untouched by the second launch.
        _await_session_actor(tab_a, user_actor, what="tab A still serves the user")
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()

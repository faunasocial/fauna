"""tier_3 e2e: web remove-account refuses an account another tab serves, and
never offers the account THIS tab serves.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*An erase refuses while a sibling serves the account* (remove-account's
``settings.remove_account_blocked_other_window`` on the Settings page's
``error-message``) and *Remove-account also refuses the account THIS instance
serves* (every switcher marks the account the instance serves — not the
registry's active pointer — as the one in use, with no remove button).

**The world.** One browser profile, two accounts U (active) and V. Tab A serves
U and holds U's engine role. Tab B switches itself to V, which moves the
registry's active pointer to V while tab A still runs on U. That is the state
in which the two keyings disagree:

1. In tab B, U is offered for removal (tab B does not serve it), and pressing it
   must REFUSE — tab A holds U's role — erasing nothing. Pre-fix the removal
   dropped U's secret slots out from under tab A.
2. In tab A, U must NOT be offered for removal: it is the account tab A serves.
   Pre-fix the switcher keyed "in use" on the registry pointer (now V), so tab A
   offered its own account for removal.
"""
from __future__ import annotations

import json
import time

import pytest

from drivers import create_driver
from tests.test_account_switcher_web import (
    ACCOUNT_PAGE_NAV,
    ADMIN_TAB,
    REMOVE_BUTTON,
    SWITCHER_ITEM,
    _read_registry_index,
    _seed_registry,
    _seed_two_accounts,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

CONVERSATIONS_NAV = {"nav": {"stack": [{"view": "conversations"}]}}
NEW_CONVERSATION_BUTTON = "new-conversation-button"
ERROR_MESSAGE = "error-message"

# Convention 14: a named, generous ceiling on latency-independent state.
BOOT_BUDGET_S = 60.0


def _wait_until(pred, *, what: str, budget_s: float = BOOT_BUDGET_S):
    """Deadline-poll ``pred`` until it returns something truthy, and return it."""
    deadline = time.monotonic() + budget_s
    last = None
    while time.monotonic() < deadline:
        try:
            last = pred()
        except Exception as e:  # noqa: BLE001 — mid-boot the element may not exist yet
            last = e
        else:
            if last:
                return last
        time.sleep(0.5)
    raise AssertionError(f"{what} did not happen within {budget_s}s; last read: {last!r}")


def _rows(driver) -> list[dict]:
    """Each switcher row's handle text and whether it offers a remove button, in
    display (add) order — read in one pass so the pairing cannot skew."""
    return driver.eval_js(
        "Array.from(document.querySelectorAll('[data-testid=\"account-switcher-item\"]'))"
        ".map(r => ({handle: (r.querySelector('[data-testid=\"account-item-handle\"]')"
        "?.textContent || '').trim(), removable: !!r.querySelector("
        "'[data-testid=\"account-remove-button\"]')}))"
    ) or []


def _session_actor(driver):
    return ((driver.get_state() or {}).get("session") or {}).get("actor_id")


def _path(driver) -> str:
    return driver.eval_js("location.pathname") or ""


def _open_account_page(driver, *, what: str) -> list[dict]:
    """Navigate ``driver`` to Account settings and return its two switcher rows.

    A tab that has just loaded sits on the bare ``/app/`` index until the root
    layout's ``routeAppRoot`` settles — after wasm is up and the pending-factory-
    reset read resolves — and then redirects to ``/app/conversations``. A nav
    patch issued inside that window races the redirect and loses when the
    redirect lands last, leaving the Account page unmounted (``count=0``). So wait
    for the tab to leave the index first: the only navigation in play is then this
    one (convention 14 — delete the race, never widen a timeout). On failure the
    message names the tab's path and its error text (convention 6)."""
    _wait_until(
        lambda: _path(driver).rstrip("/") not in ("", "/app"),
        what=f"{what}: boot routing leaving the /app index",
    )
    driver.set_state(ACCOUNT_PAGE_NAV)
    try:
        return _wait_until(
            lambda: (rows := _rows(driver)) and len(rows) == 2 and rows,
            what=f"{what}: the switcher listing both accounts",
        )
    except AssertionError as e:
        raise AssertionError(
            f"{e}; path={_path(driver)!r}, error-message={driver.get_texts(ERROR_MESSAGE)!r}"
        ) from None


@pytest.mark.feature("multiple-accounts")
def test_web_remove_account_refuses_an_account_another_tab_serves(nest_instance, spa_url):
    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )

    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        # (1) Tab A boots over the seeded profile, serving U (the active account),
        # and takes U's engine role — an ENABLED compose button is positive
        # evidence (`disabled={!manager}`).
        _seed_registry(tab_a, seed)
        tab_a.hard_reload()
        _wait_until(lambda: _session_actor(tab_a) == user_actor, what="tab A serving U")
        tab_a.set_state(CONVERSATIONS_NAV)
        tab_a.wait_for(NEW_CONVERSATION_BUTTON, timeout=BOOT_BUDGET_S)
        _wait_until(
            lambda: tab_a.is_enabled(NEW_CONVERSATION_BUTTON),
            what="tab A building its conversations manager (taking U's engine role)",
        )

        # (2) Tab B, same profile, switches ITSELF to V (row 1). The switch moves
        # the registry's active pointer to V and pins tab B there; tab A is not
        # reloaded and keeps serving U.
        tab_b = tab_a.open_same_context_tab()
        _open_account_page(tab_b, what="tab B before the switch")
        tab_b.click(SWITCHER_ITEM, index=1)
        tab_b.wait_for(ADMIN_TAB, timeout=BOOT_BUDGET_S)
        _wait_until(lambda: _session_actor(tab_b) == admin_actor, what="tab B serving V")
        assert _session_actor(tab_a) == user_actor, "tab A must still serve U"

        # (3) Tab B: U is offered (tab B does not serve it); removing it REFUSES.
        rows_b = _open_account_page(tab_b, what="tab B after switching to V")
        assert [r["removable"] for r in rows_b] == [True, False], (
            "tab B serves V, so exactly U (row 0) is offered for removal and V is not; "
            f"rows={rows_b!r}"
        )
        tab_b.click(REMOVE_BUTTON)
        refusal = _wait_until(
            lambda: next(
                (t for t in tab_b.get_texts(ERROR_MESSAGE) if "not removed" in t.lower()), None
            ),
            what="tab B's remove-account refusal on error-message",
        )
        assert "another fauna window" in refusal.lower(), (
            "the refusal must be the shared `settings.remove_account_blocked_other_window` "
            f"line; error-message read {refusal!r}"
        )
        index = _read_registry_index(tab_b) or {}
        assert user_actor in [a["actor_id"] for a in index.get("accounts", [])], (
            f"a refused removal erases nothing, but U left the registry: {index!r}"
        )
        stored = tab_b.eval_js(f"localStorage.getItem({json.dumps(f'fauna/{user_actor}/secret')})")
        assert stored == user_secret, "a refused removal must leave U's secret slot in place"

        # (4) Tab A: the account it serves is marked in use and NOT offered for
        # removal, although the registry's active pointer now names V.
        rows_a = _open_account_page(tab_a, what="tab A")
        assert [r["removable"] for r in rows_a] == [False, True], (
            "tab A serves U, so U (row 0) must not be offered for removal — keying on the "
            f"registry pointer (now V) would offer tab A its own account; rows={rows_a!r}"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()

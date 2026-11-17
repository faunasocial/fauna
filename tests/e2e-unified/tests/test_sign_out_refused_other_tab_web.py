"""tier_3 e2e: a web sign-out refuses while another tab serves the account —
the role holder's included, until the sibling is closed.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*An erase refuses while a sibling serves the account*. Sign-out erases every
account-scoped store of the browser profile, so a tab signing out while a
sibling tab still serves the account strands that sibling on an erased store —
its conversations engine keeps writing a device leaf whose credentials are
gone. Web asks at two Web Locks, as native asks at two locks: the MLS-engine
role (``fauna_mls::engine_role``) and the serving-presence lock every tab whose
identity is the account holds shared (``fauna_client_accounts::
web_serving_presence``; § Concurrent instances → *Web's serving-presence lock*).
A sign-out finds both free, or refuses **entirely** — nothing erased, no
credentials wiped, still signed in — with the shared line
``settings.sign_out_blocked_other_window`` on the Settings page's
``error-message``.

**Why the witness is a pair, in one browser profile.** Refusal alone is
satisfied by a guard that refuses everything — and the obvious probe does
exactly that in a tab that *holds* a lock, where an ``ifAvailable`` request
meets its own hold. So the test asserts every half against one world: tab B
(refused the engine, tab A holds it) is refused its sign-out; tab A — the role
holder — is refused too while tab B is open, because tab B holds the account's
identity and declares it (before the serving-presence lock, the holder's
sign-out proceeded here and tab B then won the role and ran the signed-out
account on); and once tab B is closed tab A's sign-out proceeds and erases — so
neither tab's own holds refuse it.

The world is this module's own driver seeded through ``localStorage`` (the
``test_sign_out_web.py`` shape), not ``logged_in_app``: the test ends by
erasing the profile's whole credential namespace, which must not be a shared
fixture's.
"""
from __future__ import annotations

import json

import time

import pytest

from actions import ActionLayer
from common.auth import create_actor_and_register
from common.accounts import single_account_seed
from drivers import create_driver
from helpers.web_tabs import lock_queue

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

CONVERSATIONS_NAV = {"nav": {"stack": [{"view": "conversations"}]}}
ERROR_MESSAGE = "error-message"
NEW_CONVERSATION_BUTTON = "new-conversation-button"
SIGN_OUT_CONFIRM_BUTTON = "sign-out-confirm-button"

# Convention 14: a named, generous ceiling on latency-independent state (a page
# boot plus one Web Locks round trip), never a settle-sleep.
BOOT_BUDGET_S = 60.0


def _ls(driver, key: str):
    return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")


def _registry_keys(driver) -> list[str]:
    return sorted(driver.eval_js("Object.keys(localStorage).filter(k => k.startsWith('fauna/'))"))


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


def _error_texts(driver) -> str:
    """Every ``error-message`` on the page, joined — the Settings page carries
    one per section, and the refusal paints on the page-level banner."""
    return " | ".join(driver.get_texts(ERROR_MESSAGE))


@pytest.mark.feature("account")
def test_web_sign_out_refuses_while_another_tab_serves_the_account(nest_instance, spa_url):
    actor = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    secret = bytes(actor["signing_key"]).hex()
    actor_id = actor["actor_id_hex"]

    tab_a = create_driver("web")
    tab_a.launch({
        "url": spa_url + "/app/",
        "seed_credentials": single_account_seed(secret, nest_url=spa_url),
    })
    tab_b = None
    try:
        _wait_until(
            lambda: (json.loads(_ls(tab_a, "fauna/index") or "{}").get("active") == actor_id),
            what="the boot registry read materializing the seeded account as active",
        )

        # (1) Tab A takes the account's engine role: an ENABLED compose button is
        # `disabled={!manager}`, i.e. positive evidence this tab built the engine.
        tab_a.set_state(CONVERSATIONS_NAV)
        tab_a.wait_for(NEW_CONVERSATION_BUTTON, timeout=BOOT_BUDGET_S)
        _wait_until(
            lambda: tab_a.is_enabled(NEW_CONVERSATION_BUTTON),
            what="tab A building its conversations manager (taking the engine role)",
        )

        # (2) Tab B: same profile, same account — refused the engine, which is
        # the proof it booted as this account and that tab A holds the role.
        tab_b = tab_a.open_same_context_tab()
        tab_b.set_state(CONVERSATIONS_NAV)
        _wait_until(
            lambda: "another instance" in (tab_b.get_text(ERROR_MESSAGE) or "").lower(),
            what="tab B rendering the served-elsewhere engine refusal",
        )

        # (3) Tab B signs out → REFUSED, entirely.
        ActionLayer(tab_b).settings.press_sign_out()
        refusal = _wait_until(
            lambda: "still signed in" in _error_texts(tab_b).lower() and _error_texts(tab_b),
            what="tab B's sign-out refusal on error-message",
        )
        assert "another fauna window" in refusal.lower(), (
            "the refusal must be the shared `settings.sign_out_blocked_other_window` "
            f"line every seat paints; error-message read {refusal!r}"
        )
        assert _ls(tab_b, f"fauna/{actor_id}/secret") == secret, (
            "a refused sign-out must wipe no credentials, but the identity's secret slot is gone"
        )
        assert _ls(tab_b, f"fauna/{actor_id}/secret") == secret, (
            "a refused sign-out must erase nothing, but the account's registry secret is gone"
        )
        assert (tab_b.get_state() or {}).get("session", {}).get("actor_id") == actor_id, (
            "a refused sign-out leaves the tab signed in as the same account"
        )
        # The remedy is pressing the same control again, so it must still show.
        assert tab_b.is_visible(SIGN_OUT_CONFIRM_BUTTON), (
            "the sign-out control must stay showing beside the refusal"
        )

        # (4) Tab A — the role HOLDER — is refused too while tab B is open: tab B
        # holds the account's identity without its role, and declares it on the
        # serving-presence lock. Its own role and its own serving hold do not
        # refuse it — step (5) proves that.
        ActionLayer(tab_a).settings.press_sign_out()
        holder_refusal = _wait_until(
            lambda: "still signed in" in _error_texts(tab_a).lower() and _error_texts(tab_a),
            what="tab A's sign-out refusal on error-message while tab B is open",
        )
        assert "another fauna window" in holder_refusal.lower(), (
            "the holder's refusal must be the same shared line; "
            f"error-message read {holder_refusal!r}"
        )
        assert _ls(tab_a, f"fauna/{actor_id}/secret") == secret, (
            "the holder's refused sign-out must wipe no credentials beside a serving sibling"
        )
        assert (tab_a.get_state() or {}).get("session", {}).get("actor_id") == actor_id, (
            "the holder's refused sign-out leaves it signed in as the same account"
        )

        # (5) Tab B closes — the browser lets its holds go — and the holder's
        # sign-out PROCEEDS. A probe that met its own locks would refuse forever.
        # The remedy is the whole of it: close the other tab and press the same
        # control again — the refusal leaves the confirm button showing.
        tab_b.teardown()
        tab_b = None
        # The browser lets a closed tab's holds go on its own clock: the page is
        # gone before the lock manager has dropped its shared serving hold, and a
        # press in between is refused again — rightly, the hold still stands. So
        # the press waits for the origin's own record to show tab A as the one
        # holder left (convention 14: the state, never a settle-sleep).
        serving = f"fauna.accounts.serving/{actor_id}"
        _wait_until(
            lambda: lock_queue(tab_a)["held"].count(serving) == 1,
            what=f"the browser releasing the closed tab B's {serving} hold",
        )
        assert tab_a.is_visible(SIGN_OUT_CONFIRM_BUTTON), (
            "the holder's sign-out control must stay showing beside its refusal"
        )
        tab_a.click(SIGN_OUT_CONFIRM_BUTTON)
        try:
            _wait_until(
                lambda: tab_a.is_visible("create-identity-button"),
                what="the holder's sign-out reaching onboarding once tab B closed",
            )
        except AssertionError as e:
            raise AssertionError(
                f"{e}; error-message read {_error_texts(tab_a)!r}, "
                f"locks held {lock_queue(tab_a)['held']!r}"
            ) from None
        survivors = _registry_keys(tab_a)
        assert survivors == [], (
            "with no sibling open the holder must be able to sign out (its own role "
            f"and serving hold are not another tab's); these registry keys survived: {survivors}"
        )
        assert _ls(tab_a, f"fauna/{actor_id}/secret") is None, "the holder's sign-out erased nothing"
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()

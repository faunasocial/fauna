"""tier_3 e2e: a web sign-out refuses while another tab serves the account.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*An erase refuses while a sibling serves the account*. Sign-out erases every
account-scoped store of the browser profile, so a tab signing out while a
sibling tab still serves the account strands that sibling on an erased store —
its conversations engine keeps writing a device leaf whose credentials are
gone. Web's analogue of the native instance lock is the MLS-engine role Web
Lock (``$lib/webLocks``): a sign-out finds it free, or refuses **entirely** —
nothing erased, no credentials wiped, still signed in — with the shared line
``settings.sign_out_blocked_other_window`` on the Settings page's
``error-message``.

**Why the witness is a pair, in one browser profile.** Refusal alone is
satisfied by a guard that refuses everything — and the obvious probe does
exactly that in the tab that *holds* the role, where an ``ifAvailable`` request
meets its own lock. So the test asserts both halves against one world: tab B
(refused the engine, tab A holds it) is refused its sign-out, and then tab A —
the role holder, with tab B still open — signs out and erases. Pre-fix, tab B's
sign-out erased the shared store out from under tab A.

The world is this module's own driver seeded through ``localStorage`` (the
``test_sign_out_web.py`` shape), not ``logged_in_app``: the test ends by
erasing the profile's whole credential namespace, which must not be a shared
fixture's.
"""
from __future__ import annotations

import json

from helpers import web_store
import time

import pytest

from actions import ActionLayer
from common.auth import create_actor_and_register
from drivers import create_driver

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
def test_web_sign_out_refuses_while_another_tab_holds_the_engine(nest_instance, spa_url):
    actor = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    secret = bytes(actor["signing_key"]).hex()
    actor_id = actor["actor_id_hex"]

    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_b = None
    try:
        web_store.seed_identity(tab_a, secret, nest_url=spa_url)
        tab_a.hard_reload()
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

        # (4) Tab A — the role HOLDER — signs out with tab B still open, and it
        # PROCEEDS. A probe that met its own lock would refuse here forever.
        ActionLayer(tab_a).settings.sign_out()
        survivors = _registry_keys(tab_a)
        assert survivors == [], (
            "the tab holding the engine role must be able to sign out (its own role "
            f"is not another tab's); these registry keys survived: {survivors}"
        )
        assert _ls(tab_a, f"fauna/{actor_id}/secret") is None, "the holder's sign-out erased nothing"
    finally:
        if tab_b is not None:
            tab_b.teardown()
        tab_a.teardown()

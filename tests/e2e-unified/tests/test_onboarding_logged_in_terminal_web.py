"""tier_3 e2e: web's ``LoggedIn`` wizard terminal records the home nest PER-ACTOR,
and a fully onboarded user is still in the app after a PAGE RELOAD.

``docs/goal/behavior/onboarding.md`` § Long-term store contract → *The ``LoggedIn``
terminal records the home nest PER-ACTOR* (ratified 2026-08-22): reaching
``WizardOutcome::LoggedIn`` means this identity has a home nest, and that is what
the next launch's routing row branches on. The per-actor ``fauna/<actor>/nest_url``
row is the ONLY place the home nest is recorded; without it the launch machine's
routing tuple degrades to ``(Some(secret), None, None)`` -> ``WizardAt(HandleEntry)``
(§ App-launch routing) and a completed onboarding silently re-renders the
handle-entry page, identity intact, nothing logged or surfaced.

**What this pins.** Three moments:

1. Identity-confirm calls ``accountsPersistConfirmedIdentity``, materializing the
   account index with a per-actor secret row and **no** per-actor ``nest_url`` row
   — none is known yet.
2. ``handleWizardExit``'s ``LoggedIn`` branch registers the nest URL per-actor on
   the ordinary (non-append) path, as the ``appendMode`` branch does.
3. A page reload (``identity.init()`` runs once per page load) routes the launch
   machine from those per-actor rows straight into the authenticated session,
   not back into the wizard.

**Why this test must RELOAD, and why no existing test covered it.** Web's wizard
exit is an in-SPA ``goto('/app/feed')`` with no relaunch, so the session the user
just completed works fine — a missing home-nest row surfaces on their *next* page
load. Every arrival-asserting test is therefore structurally blind to it. Smoke K
(``test_onboarding_launch_routing_smoke.py``) is additionally native-only
parametrized, so it does not run on web at all.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); web app only.
"""
from __future__ import annotations

import json
import time
from types import SimpleNamespace

import pytest

from common import create_actor_and_register
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

# tests/e2e-unified/ui.yaml § onboarding.
IDENTITY_CHOICE_IMPORT = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"

# Generous, latency-independent budgets (e2e-conventions.md convention 14): every
# wait below exits on a causal condition, never on elapsed time.
BOOT_BUDGET_S = 60.0
TERMINAL_BUDGET_S = 45.0


def _ls(driver, key: str):
    """One `localStorage` value, or None when the key is absent."""
    return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")


def _per_actor_nest_url(driver, actor_id: str):
    """The per-actor home-nest row — the source of truth the launch machine's
    `load_nest_url()` reads (`fauna-client-accounts` `nest_url_key`)."""
    return _ls(driver, f"fauna/{actor_id}/nest_url")


def _wait_per_actor_nest_url(driver, actor_id: str, budget_s: float):
    """Poll until the wizard terminal's per-actor write has landed."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        value = _per_actor_nest_url(driver, actor_id)
        if value:
            return value
        time.sleep(0.25)
    return None


@pytest.mark.feature("connect-and-sign-in")
def test_web_logged_in_terminal_survives_a_page_reload(nest_instance, spa_url):
    """A real (non-append) wizard completion on web, then a RELOAD.

    Drives the wizard's own terminal — not `set_state`, not a seeded registry —
    because the terminal hand-off is exactly what breaks. The handle-check
    *outcome* is injected (the real `start_handle_check` path over browser WS-RPC
    is covered by `test_onboarding_localhost.py`, and every outcome by
    `test_handle_entry_outcomes.py`); the mutation under test — Continue →
    `WizardOutcome::LoggedIn` → the terminal's persistence — is driven through the
    UI a user clicks (e2e-conventions.md point 8).
    """
    # (0) A pre-registered identity to import, so the handle resolves to the
    #     welcome-back `AlreadyOnNest` outcome a real returning user gets.
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    actor_id = user["actor_id_hex"]
    secret_hex = bytes(user["signing_key"]).hex()

    driver = create_driver("web")
    # A FRESH install: nothing stored, so the routing table's last row applies and
    # the wizard really runs (no append mode — `?add=1` is absent).
    driver.launch({"url": spa_url + "/app/"})
    try:
        driver.wait_for(IDENTITY_CHOICE_IMPORT, timeout=BOOT_BUDGET_S)

        # (1) Install the dial override at identity_choice, before importing. The
        #     wizard derives `nest_url` from the typed handle domain — uniform
        #     https by ratified design — so the post-terminal launch would dial
        #     `https://localhost:<port>` at a plain-HTTP tier_3 nest. Web's
        #     `set_provider_base_urls` is a query param + `hard_reload()`, and a
        #     reload mid-wizard rebuilds a fresh wizard; at identity_choice that
        #     re-boot is idempotent, so this is the one safe placement (the
        #     placement `test_account_switcher_web.py` ratified). The param
        #     survives the later `hard_reload()`, so the reload under test dials
        #     the same fixture nest.
        driver.set_provider_base_urls({"nest": nest_instance["url"]})
        driver.wait_for(IDENTITY_CHOICE_IMPORT, timeout=BOOT_BUDGET_S)

        # (2) Import the pre-registered identity (the paste-secret path). This is
        #     MOMENT 1: `accountsPersistConfirmedIdentity` materializes the account
        #     index with a per-actor secret row and no per-actor `nest_url`.
        driver.click(IDENTITY_CHOICE_IMPORT)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=TERMINAL_BUDGET_S)
        driver.clear_and_type(PASTE_SECRET_FIELD, secret_hex)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=TERMINAL_BUDGET_S)

        assert _per_actor_nest_url(driver, actor_id) is None, (
            "precondition: after identity-confirm the per-actor nest_url row must "
            "NOT exist yet — no nest is known at that moment. If it does, this "
            "test is no longer observing the wizard terminal's own write."
        )

        # (3) The welcome-back outcome → Continue lands `WizardOutcome::LoggedIn`,
        #     which runs `handleWizardExit`'s non-append terminal.
        handle = f"user@localhost:{nest_instance['port']}"
        set_handle_check_snapshot(SimpleNamespace(driver=driver), {
            "phase": "Complete",
            "outcome": {"AlreadyOnNest": {"handle_matches": True, "current_handle": handle}},
            "message": {
                "key": "onboarding.handle_check.outcome.already_on_nest_handle_matches",
                "args": {"handle": handle},
            },
            "continue_enabled": True,
            "control_checkbox_visible": False,
            "control_checkbox_checked": False,
        }, handle=handle)
        driver.click(HANDLE_CONTINUE_BUTTON)

        # (4) THE FIX, asserted at its own moment: the terminal recorded the home
        #     nest PER-ACTOR — the ONLY place it is recorded. The row is
        #     written by the non-append terminal; this assertion fails if not.
        stored = _wait_per_actor_nest_url(driver, actor_id, TERMINAL_BUDGET_S)
        assert stored, (
            "the wizard's LoggedIn terminal did not record the home nest per-actor "
            f"(fauna/{actor_id}/nest_url absent after Continue) — the per-actor "
            "row is the ONLY place the home nest is recorded (onboarding.md "
            "§ Long-term store contract)."
        )
        # The seam redirects the SOCKET, never the truth: what the terminal
        # persisted is the literal the wizard derived from the typed handle
        # domain, not the harness dial override. An override that leaked into
        # persistence would make every later launch — in a release build with no
        # override installed — dial a torn-down fixture (the shared-Rust twin is
        # `dial_override_never_reaches_the_store`).
        assert stored == f"https://localhost:{nest_instance['port']}", (
            "the terminal must persist the wizard-derived nest URL, NOT the "
            f"harness dial override; got {stored!r}"
        )

        # (5) THE RELOAD — the moment the user's loss actually surfaced, and the
        #     whole reason this case exists. `identity.init()` runs once per PAGE
        #     LOAD, not per client-side navigation, so the in-SPA `goto` the
        #     wizard exits through could never have exercised the boot routing.
        #     Nothing is cleared first: the reload must start from exactly the
        #     state a real completed onboarding leaves behind — pre-seeding a
        #     missing row would manufacture a bounce the product never causes.
        driver.hard_reload()

        # (6) The causal barrier is the ROUTING DECISION itself (convention 14 —
        #     a barrier, never a settle-sleep). After the boot the
        #     launch machine lands on exactly one of two surfaces, so polling the
        #     disjunction cannot hang on either branch and cannot pass before the
        #     boot routing has run: `session.actor_id` is set by
        #     `identity.init()`, and the wizard surface is what the
        #     degraded tuple routes to. Whichever appears first IS the verdict.
        deadline = time.monotonic() + BOOT_BUDGET_S
        bounced_to_wizard = False
        routed_into_app = False
        while time.monotonic() < deadline:
            if driver.is_visible(HANDLE_INPUT) or driver.is_visible(IDENTITY_CHOICE_IMPORT):
                bounced_to_wizard = True
                break
            try:
                if driver.get_state("session.actor_id") == actor_id:
                    routed_into_app = True
                    break
            except Exception:
                pass
            time.sleep(0.25)

        # The user-visible half of the definition of success.
        assert not bounced_to_wizard, (
            "a completed onboarding landed back in the WIZARD after a page "
            "reload — the silent re-onboarding this case exists to kill. The "
            "launch machine routed on a tuple with no nest_url "
            "(onboarding.md § App-launch routing). "
            f"per-actor row={_per_actor_nest_url(driver, actor_id)!r}"
        )
        assert routed_into_app, (
            "after the reload the app reached neither the authenticated session "
            "nor the wizard within the budget, so this run proves nothing either "
            "way — treat it as a harness failure, not a green. "
            f"per-actor row={_per_actor_nest_url(driver, actor_id)!r}"
        )

        # (7) …and the mechanical half, now that the boot routing is known to have
        #     run: the per-actor row still carries the home nest.
        assert _per_actor_nest_url(driver, actor_id), (
            "the per-actor home-nest row must outlive the reload's boot: it is "
            "the source of truth `load_nest_url()` reads."
        )
    finally:
        driver.teardown()

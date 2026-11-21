"""tier_3 e2e: on linux the wizard's window becomes the app at sign-in.

`docs/goal/behavior/onboarding.md` § Wizard exit handling → *The wizard
becomes the app in place*: at `LoggedIn` the surface the person is looking at
stays on screen and its content changes from the wizard to the signed-in app
(the launch progress view in between); an app must not destroy the wizard's
window and later create another. The same holds for an append-mode run's
switch to the new account — where the surface cannot be re-used, the new one
is created before the old one is closed.

Neither is visible in an element tree (a close and a reopen lands the same
widgets in a new window), so both journeys read the app's journal of visible
top-level windows (`fauna_e2e_agent::TOPLEVEL_WINDOWS_KEY` owns the contract),
recorded from the windows' own notifications — a gap shorter than a frame is
an entry too (convention 14):

1. First run: a fresh install signs in through the wizard; the window that
   showed the wizard is the one that shows the main window, and no entry after
   the first is empty.
2. Append mode: "Add account" from a signed-in session, the wizard signs the
   second identity in and the app switches to it; no entry is empty from the
   first window to the switched session's main window.

Linux only: the other six apps swap content inside one surface by
construction (one terminal, one tab, one activity, one window) and publish no
window journal. tier_3: a real `fauna-nest` binary (``nest_instance``).
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import await_session_actor

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
]

TOPLEVEL_WINDOWS_KEY = "toplevel_windows"
# The widget names linux gives its two shell windows (not ui.yaml ids: a
# window is no element; these name the window objects the journal lists).
WIZARD_WINDOW = "fauna-onboarding-window"
MAIN_WINDOW = "fauna-main-window"

ADD_BUTTON = "account-add-button"
SWITCHER_LIST = "account-switcher-list"
IMPORT_IDENTITY_BUTTON = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _journal(driver):
    """The app's window journal, or a loud refusal (convention 11)."""
    value = driver.get_state(TOPLEVEL_WINDOWS_KEY)
    assert isinstance(value, dict) and isinstance(value.get("journal"), list), (
        f"the app publishes no `{TOPLEVEL_WINDOWS_KEY}` journal: {value!r}"
    )
    return value


def _wait_main_window_shown(driver, *, not_id=None, budget_s=APP_RELAUNCH_S):
    """Wait until a visible window carries the main window's name (and, given
    ``not_id``, is not that window); return the journal then."""
    def shown(state):
        now = (state.get(TOPLEVEL_WINDOWS_KEY) or {}).get("now") or []
        return any(w.get("name") == MAIN_WINDOW and w.get("id") != not_id for w in now)

    state = driver.wait_for_state(shown, timeout=budget_s)
    return state[TOPLEVEL_WINDOWS_KEY]


def _assert_never_empty_after(journal, start_seq, what):
    """No journal entry from ``start_seq`` on lists zero visible windows."""
    for entry in journal["journal"]:
        if entry["seq"] >= start_seq and not entry["visible"]:
            pytest.fail(
                f"{what}: the app had NO visible window at journal entry "
                f"{entry['seq']} — a close and a reopen, the gap onboarding.md "
                f"§ Wizard exit handling forbids. Journal: {journal['journal']}"
            )


def _first_seq_showing(journal, name):
    for entry in journal["journal"]:
        for w in entry["visible"]:
            if w["name"] == name:
                return entry["seq"], w["id"]
    pytest.fail(f"no journal entry shows a `{name}` window: {journal['journal']}")


def _sign_in_through_wizard(driver, nest_instance, secret_hex, handle):
    """Import ``secret_hex`` on the wizard on screen and finish with a
    welcome-back handle check, so Continue lands `WizardOutcome::LoggedIn`."""
    driver.click(IMPORT_IDENTITY_BUTTON)
    driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
    driver.clear_and_type(PASTE_SECRET_FIELD, secret_hex)
    driver.click(IMPORT_SUBMIT_BUTTON)
    driver.wait_for(HANDLE_INPUT, timeout=15)
    # The launch after LoggedIn dials the store's uniform-https URL, which this
    # plain-HTTP nest cannot serve; the override covers both halves.
    driver.set_provider_base_urls({"nest": nest_instance["url"]})
    set_handle_check_snapshot(SimpleNamespace(driver=driver), {
        "phase": "Complete",
        "outcome": {"AlreadyOnNest": {"handle_matches": True, "current_handle": handle}},
        "message": {
            "key": "onboarding.handle_check.outcome.already_on_nest_handle_matches",
            "args": {"handle": handle},
        },
        "continue_enabled": True,
    }, handle=handle)
    driver.click(HANDLE_CONTINUE_BUTTON)


@pytest.mark.feature("connect-and-sign-in")
def test_linux_first_run_wizard_window_becomes_the_main_window(
    nest_instance, linux_app_path, request
):
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    secret_hex = bytes(user["signing_key"]).hex()

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        _sign_in_through_wizard(
            driver, nest_instance, secret_hex, f"user@localhost:{nest_instance['port']}"
        )
        await_session_actor(driver, user["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the first-run sign-in")
        journal = _wait_main_window_shown(driver)

        wizard_seq, wizard_id = _first_seq_showing(journal, WIZARD_WINDOW)
        _assert_never_empty_after(journal, wizard_seq, "first-run sign-in")
        mains = [w for w in journal["now"] if w["name"] == MAIN_WINDOW]
        assert [w["id"] for w in mains] == [wizard_id], (
            "the window that showed the wizard must be the one that shows the app "
            f"(wizard window {wizard_id}); visible now: {journal['now']}; "
            f"journal: {journal['journal']}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("connect-and-sign-in")
def test_linux_add_account_switch_never_leaves_no_window(
    nest_instance, linux_app_path, request
):
    admin_sk = nest_instance["admin"]["signing_key"]
    first = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    seed = build_registry_seed(
        [{"actor_id": first["actor_id_hex"],
          "secret_hex": bytes(first["signing_key"]).hex(),
          "nest_url": nest_instance["url"],
          "device_id": "in-place-first", "handle": "user"}],
        active=first["actor_id_hex"],
    )

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        await_session_actor(driver, first["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the seeded launch")
        before = _wait_main_window_shown(driver)
        first_main = next(w["id"] for w in before["now"] if w["name"] == MAIN_WINDOW)
        start_seq = before["journal"][-1]["seq"]

        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_LIST, timeout=30)
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        _sign_in_through_wizard(
            driver, nest_instance, bytes(second["signing_key"]).hex(),
            f"user2@localhost:{nest_instance['port']}",
        )
        await_session_actor(driver, second["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the append switch")
        after = _wait_main_window_shown(driver, not_id=first_main)
        _assert_never_empty_after(after, start_seq, "append-mode switch")
    finally:
        driver.teardown()

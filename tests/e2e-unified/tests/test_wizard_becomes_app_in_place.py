"""tier_3 e2e: the one surface shows the launch progress view or the app from
the wizard's exit on, never nothing — tui, web, android, macos and ios.

`docs/goal/behavior/onboarding.md` § Wizard exit handling → *The wizard
becomes the app in place*: at `LoggedIn` the surface the person is looking at
(the terminal, the tab, the activity, the window) stays on screen and its content changes from the
wizard to the signed-in app; while the launch work between the two runs, that
same surface shows the launch progress view, never nothing. The same holds for
an append-mode run's switch to the new account.

A read of the element tree samples, and a blank frame shorter than a poll
passes it, so both journeys read the app's journal of what its root showed
(`fauna_e2e_agent::ROOT_SURFACE_KEY` owns the contract), fed by the app's
paint path once per painted frame — a blank frame is an entry however briefly
it stood (convention 14):

1. First run: a fresh install signs in through the wizard; from the wizard's
   first frame to the signed-in app, no entry is nothing, and after the
   wizard's last entry the root shows only the launch progress view or the app.
2. Append mode: "Add account" from a signed-in session, the wizard signs a
   second identity in and the app switches to it; the same holds from the
   first session's page to the second session's.

On macos the rule is about the WINDOW as well: the window that showed the
wizard is the window that shows the app, with no moment at which the app has
no window. No element read can see a close and a reopen (the same elements
land in a new window), so both macos journeys also read the app's journal of
visible windows (`fauna_e2e_agent::TOPLEVEL_WINDOWS_KEY`).

linux, whose shell has several windows, has its own witness reading its window
journal (`test_wizard_becomes_app_in_place_linux.py`). tier_3: a real
`fauna-nest` binary (``nest_instance``).
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _build_app_config, _seeded_environment, get_available_apps
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import await_session_actor, wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.android,
    pytest.mark.macos, pytest.mark.ios,
]

ROOT_SURFACE_KEY = "root_surface"
TOPLEVEL_WINDOWS_KEY = "toplevel_windows"
WIZARD = "wizard"
LAUNCH_PROGRESS = "launch_progress"
APP = "app"

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

IN_PLACE_APPS = ("tui", "web", "android", "macos", "ios")


@pytest.fixture(params=[a for a in get_available_apps() if a in IN_PLACE_APPS])
def app_name(request):
    """tui, web, android, macos or ios, as `--app` selects them — launched by each
    journey itself, fresh or seeded, which the session-cached `app` cannot be."""
    return request.param


def _journal(driver) -> dict:
    """The app's root-surface journal, or a loud refusal (convention 11)."""
    value = driver.get_state(ROOT_SURFACE_KEY)
    assert isinstance(value, dict) and isinstance(value.get("journal"), list), (
        f"the app publishes no `{ROOT_SURFACE_KEY}` journal: {value!r}"
    )
    return value


def _windows(driver) -> dict:
    """The app's visible-window journal, or a loud refusal (convention 11)."""
    value = driver.get_state(TOPLEVEL_WINDOWS_KEY)
    assert isinstance(value, dict) and isinstance(value.get("journal"), list), (
        f"the app publishes no `{TOPLEVEL_WINDOWS_KEY}` journal: {value!r}"
    )
    return value


def _assert_one_window_kept(windows: dict, start_seq: int | None, what: str) -> None:
    """From ``start_seq`` on (``None``: from the first entry listing a window),
    no entry lists zero windows, and the window visible now is the one that
    was visible at the start — never a close and a reopen."""
    entries = windows["journal"]
    if start_seq is None:
        shown = [e for e in entries if e["visible"]]
        assert shown, f"{what}: no journal entry lists a visible window: {entries}"
        start_seq = shown[0]["seq"]
    window = [e for e in entries if e["seq"] >= start_seq]
    assert window and window[0]["seq"] == start_seq, (
        f"{what}: the window journal no longer holds entry {start_seq}: {entries}"
    )
    for entry in window:
        if not entry["visible"]:
            pytest.fail(
                f"{what}: the app had NO visible window at journal entry "
                f"{entry['seq']} — the gap onboarding.md § Wizard exit handling "
                f"forbids. Journal: {window}"
            )
    first_ids = [w["id"] for w in window[0]["visible"]]
    now_ids = [w["id"] for w in windows["now"]]
    assert len(first_ids) == 1 and now_ids == first_ids, (
        f"{what}: the window that showed the wizard ({window[0]['visible']}) is "
        f"not the one showing the app ({windows['now']}) — a close and a reopen. "
        f"Journal: {window}"
    )


def _wait_showing_app(driver, budget_s=APP_RELAUNCH_S) -> dict:
    """Wait until the newest painted frame showed the app; return the journal."""
    seen: dict = {}

    def showing_app():
        seen.update(_journal(driver))
        return seen.get("now") == APP

    wait_until(showing_app, budget_s, interval=0.5,
               diagnose=lambda: f"the root never showed the app; journal: {seen!r}")
    return seen


def _assert_in_place(journal: dict, start_seq: int, what: str) -> None:
    """From ``start_seq`` on: the wizard is on the record, no entry is nothing,
    and after the wizard's last entry only the launch progress view or the app
    is shown, the app last."""
    entries = [e for e in journal["journal"] if e["seq"] >= start_seq]
    assert entries and entries[0]["seq"] == start_seq, (
        f"{what}: the journal no longer holds entry {start_seq} (it keeps the "
        f"newest only): {journal['journal']}"
    )
    wizard_seqs = [e["seq"] for e in entries if e["showing"] == WIZARD]
    assert wizard_seqs, f"{what}: no entry shows the wizard: {entries}"
    for entry in entries:
        if entry["seq"] >= wizard_seqs[0] and entry["showing"] is None:
            pytest.fail(
                f"{what}: the app painted NOTHING at journal entry {entry['seq']} "
                "— the gap onboarding.md § Wizard exit handling forbids. "
                f"Journal: {entries}"
            )
    after = [e for e in entries if e["seq"] > wizard_seqs[-1]]
    stray = [e for e in after if e["showing"] not in (LAUNCH_PROGRESS, APP)]
    assert not stray, (
        f"{what}: between the wizard and the app the root showed {stray} — only "
        f"the launch progress view or the app may stand there. Journal: {entries}"
    )
    assert after and after[-1]["showing"] == APP, (
        f"{what}: the journal does not end on the app: {entries}"
    )


def _point_wizard_at_nest(driver, dial_url) -> None:
    """Point the wizard on screen at this plain-HTTP nest, before any step:
    the launch after LoggedIn dials the store's uniform-https URL, which the
    nest cannot serve, and the override covers both halves. Web applies it by
    reloading the page, which rebuilds the wizard — so it goes first, while
    the wizard holds nothing yet (an append wizard keeps its imported secret
    on its machine alone)."""
    driver.set_provider_base_urls({"nest": dial_url})
    driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=60)


def _sign_in_through_wizard(driver, secret_hex, handle):
    """Import ``secret_hex`` on the wizard on screen and finish with a
    welcome-back handle check, so Continue lands `WizardOutcome::LoggedIn`."""
    driver.click(IMPORT_IDENTITY_BUTTON)
    driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
    driver.clear_and_type(PASTE_SECRET_FIELD, secret_hex)
    driver.click(IMPORT_SUBMIT_BUTTON)
    driver.wait_for(HANDLE_INPUT, timeout=15)
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


def _launch(app_name, nest_instance, request, seed=None):
    """Launch ``app_name`` fresh (or over ``seed``); returns ``(driver, the
    URL the app dials for the nest)`` — web dials the SPA proxy it is served
    from, every native app the nest itself."""
    config = _build_app_config(app_name, nest_instance, request)
    driver = create_driver(app_name)
    dial_url = request.getfixturevalue("spa_url") if driver.is_web() else nest_instance["url"]
    if seed is not None:
        config["seed_credentials"] = seed(dial_url)
    config["environment"] = {
        **config.get("environment", {}),
        **_seeded_environment(request, nest_instance),
    }
    driver.launch(config)
    return driver, dial_url


@pytest.mark.feature("connect-and-sign-in")
def test_first_run_sign_in_never_leaves_the_surface_showing_nothing(
    app_name, nest_instance, request
):
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    driver, dial_url = _launch(app_name, nest_instance, request)
    try:
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=60)
        _point_wizard_at_nest(driver, dial_url)
        _sign_in_through_wizard(
            driver, bytes(user["signing_key"]).hex(),
            f"user@localhost:{nest_instance['port']}",
        )
        await_session_actor(driver, user["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the first-run sign-in")
        journal = _wait_showing_app(driver)
        first = journal["journal"][0]["seq"]
        _assert_in_place(journal, first, f"{app_name} first-run sign-in")
        if app_name == "macos":
            _assert_one_window_kept(_windows(driver), None, "macos first-run sign-in")
    finally:
        driver.teardown()


@pytest.mark.feature("connect-and-sign-in")
def test_add_account_switch_never_leaves_the_surface_showing_nothing(
    app_name, nest_instance, request
):
    admin_sk = nest_instance["admin"]["signing_key"]
    first = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    def seed(dial_url):
        return build_registry_seed(
            [{"actor_id": first["actor_id_hex"],
              "secret_hex": bytes(first["signing_key"]).hex(),
              "nest_url": dial_url,
              "device_id": "in-place-first", "handle": "user"}],
            active=first["actor_id_hex"],
        )

    driver, dial_url = _launch(app_name, nest_instance, request, seed=seed)
    try:
        await_session_actor(driver, first["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the seeded launch")
        before = _wait_showing_app(driver)
        start_seq = before["journal"][-1]["seq"]
        window_seq = (_windows(driver)["journal"][-1]["seq"]
                      if app_name == "macos" else None)

        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_LIST, timeout=30)
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        _point_wizard_at_nest(driver, dial_url)
        if driver.is_web():
            # The override's reload started a new page, and with it a new
            # journal: the window is anchored on that page's first frame. The
            # page the switch leaves is the reloaded append wizard's.
            start_seq = _journal(driver)["journal"][0]["seq"]
        _sign_in_through_wizard(
            driver, bytes(second["signing_key"]).hex(),
            f"user2@localhost:{nest_instance['port']}",
        )
        await_session_actor(driver, second["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the append switch")
        after = _wait_showing_app(driver)
        _assert_in_place(after, start_seq, f"{app_name} append-mode switch")
        if window_seq is not None:
            _assert_one_window_kept(_windows(driver), window_seq, "macos append-mode switch")
    finally:
        driver.teardown()

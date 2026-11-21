"""tier_3 e2e: a finished wizard holds on the launch until the session is
signed in — never a stranded signed-in shell.

`docs/goal/behavior/onboarding.md` § Wizard exit handling → *The finished
wizard holds until the session is signed in*: the step from `LoggedIn` to the
app is a launch — the same `LaunchMachine::start` and verdict routing as a cold
launch. The app shows only on that launch's `Online` verdict; while it waits
the surface is `launch_progress`, and when the connection cannot be made it is
`launch_retry`, whose Retry re-runs the launch in place. An append-mode finish
is the same launch for the new account.

The fixture is the one the same doc's § App-launch routing → *What a completed
wizard hands the launch path* describes: a completion driven against a
plain-HTTP nest records the `https://` URL the typed handle derives, which that
nest cannot serve, so the authenticated connection fails. Each journey:

1. finishes the wizard with no dial override installed — the launch fails, and
   from the wizard's last frame the root shows only `launch_progress` or
   `launch_retry`, never the app;
2. presses Retry with the nest still undialable — `launch_retry` again, a fresh
   `launch_progress` between, never the app;
3. installs the dial override (the nest is reachable now), presses Retry — the
   app, signed in as the account the wizard finished.

The root's history is read from the app's per-frame journal
(`fauna_e2e_agent::ROOT_SURFACE_KEY`), as `test_wizard_becomes_app_in_place.py`
does: a shell painted for one frame between two launch surfaces is an entry.
tier_3: a real `fauna-nest` binary (``nest_instance``).
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _build_app_config, _seeded_environment, get_available_apps
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import require_surface
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import await_session_actor, wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux,
    pytest.mark.windows, pytest.mark.android, pytest.mark.macos, pytest.mark.ios,
]

ROOT_SURFACE_KEY = "root_surface"
WIZARD = "wizard"
LAUNCH_PROGRESS = "launch_progress"
LAUNCH_RETRY = "launch_retry"
APP = "app"

RETRY_BUTTON = "launch-retry-button"
TRANSIENT_ERROR = "launch-transient-error"
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

# The surface registry (tests/e2e-unified/surfaces.toml) says which apps hold.
FIRST_RUN_SURFACE = "wizard-first-run-held-launch"
APPEND_SURFACE = "wizard-append-held-launch"


@pytest.fixture(params=get_available_apps())
def app_name(request):
    """Every app `--app` selects — launched by each journey itself, fresh or
    seeded, which the session-cached `app` cannot be."""
    return request.param


def _journal(driver) -> dict:
    """The app's root-surface journal, or a loud refusal (convention 11)."""
    value = driver.get_state(ROOT_SURFACE_KEY)
    assert isinstance(value, dict) and isinstance(value.get("journal"), list), (
        f"the app publishes no `{ROOT_SURFACE_KEY}` journal: {value!r}"
    )
    return value


def _wait_showing(driver, surface: str, what: str, budget_s=APP_RELAUNCH_S) -> dict:
    """Wait until the newest painted frame showed ``surface``; return the
    journal. Fails early, with the journal, once the app is shown instead."""
    seen: dict = {}

    def showing():
        seen.update(_journal(driver))
        if surface != APP and seen.get("now") == APP:
            pytest.fail(
                f"{what}: the root shows the signed-in app over a launch that "
                "never reached Online — the stranded shell onboarding.md "
                f"§ Wizard exit handling forbids. Journal: {seen['journal']}"
            )
        return seen.get("now") == surface

    wait_until(showing, budget_s, interval=0.5,
               diagnose=lambda: f"{what}: the root never showed {surface}; "
                                f"journal: {seen!r}; "
                                f"{driver.diagnose(TRANSIENT_ERROR)}")
    return seen


def _assert_held(journal: dict, after_seq: int, what: str) -> int:
    """After entry ``after_seq``: only `launch_progress` and `launch_retry`,
    at least one `launch_progress`, ending on `launch_retry`. Returns the
    newest entry's seq."""
    entries = [e for e in journal["journal"] if e["seq"] > after_seq]
    assert entries, f"{what}: no entry after {after_seq}: {journal['journal']}"
    shown = [e for e in entries if e["showing"] == APP]
    assert not shown, (
        f"{what}: the root showed the signed-in app at {shown} before the launch "
        f"reached Online. Journal: {entries}"
    )
    stray = [e for e in entries if e["showing"] not in (LAUNCH_PROGRESS, LAUNCH_RETRY)]
    assert not stray, (
        f"{what}: while the launch held, the root showed {stray} — only "
        f"`launch_progress` or `launch_retry` may stand there. Journal: {entries}"
    )
    assert any(e["showing"] == LAUNCH_PROGRESS for e in entries), (
        f"{what}: no `launch_progress` frame — the launch never ran. Journal: {entries}"
    )
    assert entries[-1]["showing"] == LAUNCH_RETRY, (
        f"{what}: the journal does not end on `launch_retry`: {entries}"
    )
    return entries[-1]["seq"]


def _assert_still_held(journal: dict, after_seq: int, what: str) -> None:
    """After entry ``after_seq`` (which may have no successor): only
    `launch_progress` and `launch_retry`, and the root on `launch_retry` now."""
    entries = [e for e in journal["journal"] if e["seq"] > after_seq]
    stray = [e for e in entries if e["showing"] not in (LAUNCH_PROGRESS, LAUNCH_RETRY)]
    assert not stray, (
        f"{what}: the root showed {stray} after a Retry that could not connect — "
        f"only `launch_progress` or `launch_retry` may stand there. "
        f"Journal: {journal['journal']}"
    )
    assert journal.get("now") == LAUNCH_RETRY, (
        f"{what}: the root is not on `launch_retry`: {journal!r}"
    )


def _last_wizard_seq(journal: dict, after_seq: int, what: str) -> int:
    wizard = [e["seq"] for e in journal["journal"]
              if e["seq"] > after_seq and e["showing"] == WIZARD]
    assert wizard, f"{what}: no wizard entry after {after_seq}: {journal['journal']}"
    return wizard[-1]


def _sign_in_through_wizard(driver, secret_hex, handle):
    """Import ``secret_hex`` on the wizard on screen and finish with a
    welcome-back handle check, so Continue lands `WizardOutcome::LoggedIn`
    with the https URL ``handle`` derives — which this plain-HTTP nest
    cannot serve until the dial override is installed."""
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


def _hold_retry_then_sign_in(driver, dial_url, after_seq, actor_hex, what):
    """Steps 1–3 of the module docstring, from journal entry ``after_seq``."""
    journal = _wait_showing(driver, LAUNCH_RETRY, f"{what}, the finished wizard")
    wizard_seq = _last_wizard_seq(journal, after_seq, what)
    held = _assert_held(journal, wizard_seq, f"{what}, the finished wizard")

    # The agent's Retry may settle inside one handler, painting no frame
    # between the two `launch_retry`s, so the journal need not grow: what must
    # hold is that nothing but the two launch surfaces follows the first hold.
    driver.click(RETRY_BUTTON)
    journal = _wait_showing(driver, LAUNCH_RETRY, f"{what}, Retry on a dead nest")
    _assert_still_held(journal, held, f"{what}, Retry on a dead nest")

    driver.set_provider_base_urls({"nest": dial_url})
    driver.click(RETRY_BUTTON)
    await_session_actor(driver, actor_hex, budget_s=APP_RELAUNCH_S,
                        what=f"{what}, Retry on a reachable nest")
    _wait_showing(driver, APP, f"{what}, Retry on a reachable nest")


def _launch(app_name, nest_instance, request, seed=None):
    """Launch ``app_name`` fresh (or over ``seed``); returns ``(driver, the
    URL the app dials for the nest)``."""
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
def test_first_run_wizard_holds_on_the_launch_until_signed_in(
    app_name, nest_instance, request
):
    require_surface(app_name, FIRST_RUN_SURFACE)
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    driver, dial_url = _launch(app_name, nest_instance, request)
    try:
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=60)
        start = _journal(driver)["journal"][0]["seq"] - 1
        _sign_in_through_wizard(
            driver, bytes(user["signing_key"]).hex(),
            f"user@localhost:{nest_instance['port']}",
        )
        _hold_retry_then_sign_in(driver, dial_url, start, user["actor_id_hex"],
                                 f"{app_name} first run")
    finally:
        driver.teardown()


@pytest.mark.feature("connect-and-sign-in")
def test_append_finish_holds_on_the_new_accounts_launch_until_signed_in(
    app_name, nest_instance, request
):
    require_surface(app_name, APPEND_SURFACE)
    admin_sk = nest_instance["admin"]["signing_key"]
    first = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    def seed(dial_url):
        return build_registry_seed(
            [{"actor_id": first["actor_id_hex"],
              "secret_hex": bytes(first["signing_key"]).hex(),
              "nest_url": dial_url,
              "device_id": "held-launch-first", "handle": "user"}],
            active=first["actor_id_hex"],
        )

    driver, dial_url = _launch(app_name, nest_instance, request, seed=seed)
    try:
        await_session_actor(driver, first["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the seeded launch")
        before = _wait_showing(driver, APP, "the seeded launch")
        start = before["journal"][-1]["seq"]

        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_LIST, timeout=30)
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        _sign_in_through_wizard(
            driver, bytes(second["signing_key"]).hex(),
            f"user2@localhost:{nest_instance['port']}",
        )
        _hold_retry_then_sign_in(driver, dial_url, start, second["actor_id_hex"],
                                 f"{app_name} append finish")
    finally:
        driver.teardown()


SWITCHER_ITEM = "account-switcher-item"
SWITCHER_SURFACE = "launch-retry-account-switcher"


@pytest.mark.feature("connect-and-sign-in")
def test_append_finish_launch_retry_switches_back_to_the_previous_account(
    app_name, nest_instance, request
):
    """The way back from a failed append finish: on the new account's
    `launch_retry` the account switcher stands (the device holds two
    accounts), and picking the first account — seeded on the reachable nest —
    runs the launch as it and signs it in (onboarding.md § Wizard exit
    handling → *The finished wizard holds*: "the way back to the previous
    account is the account switcher")."""
    require_surface(app_name, APPEND_SURFACE)
    require_surface(app_name, SWITCHER_SURFACE)
    admin_sk = nest_instance["admin"]["signing_key"]
    first = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    def seed(dial_url):
        return build_registry_seed(
            [{"actor_id": first["actor_id_hex"],
              "secret_hex": bytes(first["signing_key"]).hex(),
              "nest_url": dial_url,
              "device_id": "held-launch-first", "handle": "user"}],
            active=first["actor_id_hex"],
        )

    driver, _dial_url = _launch(app_name, nest_instance, request, seed=seed)
    what = f"{app_name} append finish, the way back"
    try:
        await_session_actor(driver, first["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what="the seeded launch")
        before = _wait_showing(driver, APP, "the seeded launch")
        start = before["journal"][-1]["seq"]

        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_LIST, timeout=30)
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        _sign_in_through_wizard(
            driver, bytes(second["signing_key"]).hex(),
            f"user2@localhost:{nest_instance['port']}",
        )
        journal = _wait_showing(driver, LAUNCH_RETRY, f"{what}, the finished wizard")
        _assert_held(journal, _last_wizard_seq(journal, start, what),
                     f"{what}, the finished wizard")

        driver.wait_for(SWITCHER_LIST, timeout=15)
        count = driver.count(SWITCHER_ITEM)
        assert count == 2, (
            f"{what}: `launch_retry` lists {count} {SWITCHER_ITEM} rows, not the "
            f"device's two accounts; {driver.diagnose(SWITCHER_LIST)}"
        )
        # Registry order: the seeded first account is row 0 (its global
        # occurrence — a row anchor is not scoped within itself).
        driver.click(SWITCHER_ITEM, index=0)
        await_session_actor(driver, first["actor_id_hex"], budget_s=APP_RELAUNCH_S,
                            what=f"{what}, the switch onto the first account")
        _wait_showing(driver, APP, f"{what}, the switch onto the first account")
    finally:
        driver.teardown()

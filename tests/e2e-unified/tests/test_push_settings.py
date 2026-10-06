"""tier_3 e2e: Settings → Push notifications — the outcome a user can see.

`docs/features/notifications.md` outcome 9 — *push you turn off in the app stays
off, and push follows whoever is signed in* — and `docs/features/
multiple-accounts.md` outcome 11 — *switching identity leaves this device's
notifications on or off, exactly as you set them*. Owners: `docs/goal/
architecture/apps/common.md` § Push Notifications → *Registration* (the install
intent bit; a re-arm never opts a device in; the three leave-shapes) and
`docs/goal/ui/settings.md` § Push notifications (one toggle, the install's
opt-in — never the OS permission).

tui leads (the desktops' `ws-device` transport: the app subscribes, the sync
agent posts while the app is closed — `common.md` § Transports); the Apple
targets (APNs) follow with the one journey their entitlement gate allows — the
toggle that cannot register settling back off. Every
mutation is the app's own toggle, switch and sign-out (convention 8); the
persisted truth is read back through the nest's test-hooks surface
(`GET /api/v1/test/push/subscriptions`), the way the admin toggle tests read
theirs — never the app's word for it.

Latency-independence (convention 14): each "no row" assertion follows a causal
anchor in the same app — a row this session's own toggle wrote and then
removed, or the toggle reading the stored bit off after a relaunch, which is
the very value the launch's re-arm reads.
"""
from __future__ import annotations

import json
import urllib.parse
import urllib.request

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

# The platform marker is per test: the tui journeys launch tui themselves, the
# apple leg at the bottom rides `logged_in_app`.
pytestmark = [pytest.mark.tier_3]

TOGGLE = "push-notifications-opt-in-toggle"
SECTION = "push-notifications-section"
ERROR = "push-notifications-error"
# Both Apple targets host the control on the `general` settings slot (macOS's
# General sub-page, iOS's Notifications page — `settings.md` § Push
# notifications).
GENERAL_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}
}
SWITCHER_ITEM = "account-switcher-item"
SWITCHER_LIST = "account-switcher-list"
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _rows(nest_url: str, actor: str) -> list[dict]:
    """The actor's push rows, as the nest stores them."""
    q = urllib.parse.urlencode({"actor_id": actor})
    with urllib.request.urlopen(
        f"{nest_url}/api/v1/test/push/subscriptions?{q}", timeout=RPC_ROUNDTRIP_S
    ) as r:
        return json.load(r)


def _wait_rows(nest_url: str, actor: str, want: int) -> list[dict]:
    """Poll until the actor holds exactly ``want`` rows; return them."""
    seen: list[list[dict]] = []

    def settled():
        rows = _rows(nest_url, actor)
        seen.append(rows)
        return len(rows) == want

    wait_until(
        settled,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"rows for {actor[:12]}…: {seen[-1] if seen else None!r}",
    )
    return seen[-1]


def _open_account_page(driver) -> None:
    driver.set_state(ACCOUNT_PAGE_NAV)
    driver.wait_for(SECTION, timeout=15)


def _toggle_on(driver) -> bool:
    return driver.get_attr(TOGGLE, "state") == "on"


def _diagnose(driver) -> str:
    """Self-diagnosis (convention 6): the inline line, if any."""
    try:
        return f"push-notifications-error={driver.get_text('push-notifications-error')!r}"
    except Exception:  # noqa: BLE001 - absent is the common case
        return "no push-notifications-error"


def _register_user(nest_instance) -> tuple[str, str]:
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    return user["actor_id_hex"], bytes(user["signing_key"]).hex()


def _launch(nest_instance, tui_app_path, request, tmp_path, seed, keyring_app):
    """Launch over a store and config base pinned under ``tmp_path``, so a
    relaunch reads the same install (the intent bit is install-scoped and
    lives in the config base)."""
    config = {
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "xdg_base": str(tmp_path / "xdg"),
        "credential_dir": str(tmp_path / "creds"),
        "keyring_app": keyring_app,
        "environment": _seeded_environment(request, nest_instance),
    }
    driver = create_driver("tui")
    driver.launch(config)
    driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
    )
    return driver, config


@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_push_turned_off_stays_off_across_a_relaunch(
    nest_instance, tui_app_path, request, tmp_path
):
    url = nest_instance["url"]
    actor, secret = _register_user(nest_instance)
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": url,
          "device_id": "push-off-stays-off", "handle": "pushuser"}],
        active=actor,
    )
    driver, config = _launch(
        nest_instance, tui_app_path, request, tmp_path, seed, "push-off-stays-off"
    )
    try:
        _open_account_page(driver)
        assert not _toggle_on(driver), "a fresh install is never opted in"

        # On → this install's ws-device row exists.
        driver.click(TOGGLE)
        rows = _wait_rows(url, actor, 1)
        assert rows[0]["transport"] == "ws-device", rows
        assert _toggle_on(driver), _diagnose(driver)

        # Off → the row is gone and the toggle reads off.
        driver.click(TOGGLE)
        _wait_rows(url, actor, 0)
        assert not _toggle_on(driver), _diagnose(driver)

        # Relaunch the SAME install: off stays off — the launch's re-arm reads the
        # bit the toggle renders, and must not restore the row.
        assert driver.preserve_state_across_relaunch()
        driver.teardown()
        relaunch = dict(config)
        relaunch.pop("seed_credentials", None)
        driver.launch(relaunch)
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _open_account_page(driver)
        assert not _toggle_on(driver), "the relaunch must not read the install as opted in"
        assert _rows(url, actor) == [], "a re-arm opted a device in that the user turned off"
    finally:
        driver.teardown()


@pytest.mark.tui
@pytest.mark.feature("notifications")
@pytest.mark.feature("multiple-accounts")
def test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting(
    nest_instance, tui_app_path, request, tmp_path
):
    url = nest_instance["url"]
    first, first_secret = _register_user(nest_instance)
    second, second_secret = _register_user(nest_instance)
    seed = build_registry_seed(
        [
            {"actor_id": first, "secret_hex": first_secret, "nest_url": url,
             "device_id": "push-follow-first", "handle": "first"},
            {"actor_id": second, "secret_hex": second_secret, "nest_url": url,
             "device_id": "push-follow-second", "handle": "second"},
        ],
        active=first,
    )
    driver, _config = _launch(
        nest_instance, tui_app_path, request, tmp_path, seed, "push-follows"
    )
    try:
        # Opt in as the first identity.
        _open_account_page(driver)
        driver.click(TOGGLE)
        _wait_rows(url, first, 1)

        # Switch to the second (switcher row 1): the outgoing identity's row is
        # dropped, the incoming one is re-armed with NO Settings visit, and the
        # toggle — this device's setting — is still on (multiple-accounts 11).
        driver.wait_for(SWITCHER_LIST, timeout=15)
        driver.click(SWITCHER_ITEM, index=1)
        _wait_rows(url, second, 1)
        _wait_rows(url, first, 0)
        _open_account_page(driver)
        assert _toggle_on(driver), "switching identity turned this device's push off"

        # Off as the second, then switch back: off stays off for the first too.
        driver.click(TOGGLE)
        _wait_rows(url, second, 0)
        assert not _toggle_on(driver), _diagnose(driver)
        driver.wait_for(SWITCHER_LIST, timeout=15)
        driver.click(SWITCHER_ITEM, index=0)
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
        )
        _open_account_page(driver)
        assert not _toggle_on(driver), "switching identity turned this device's push on"
        assert _rows(url, first) == [], "a switch opted the incoming identity in"
    finally:
        driver.teardown()


@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_sign_out_drops_the_row_and_the_next_sign_in_rearms(
    nest_instance, tui_app_path, request, tmp_path
):
    from actions.settings import SettingsActions

    url = nest_instance["url"]
    actor, secret = _register_user(nest_instance)
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": url,
          "device_id": "push-sign-out", "handle": "signout"}],
        active=actor,
    )
    driver, config = _launch(
        nest_instance, tui_app_path, request, tmp_path, seed, "push-sign-out"
    )
    try:
        _open_account_page(driver)
        driver.click(TOGGLE)
        _wait_rows(url, actor, 1)

        # Sign-out drops the leaving identity's row…
        SettingsActions(driver).sign_out()
        _wait_rows(url, actor, 0)

        # …and keeps this install's opt-in: the next sign-in on the same install
        # re-arms with no Settings visit and no prompt.
        assert driver.preserve_state_across_relaunch()
        driver.teardown()
        driver.launch(config)  # re-seeds the same identity over the same config base
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_rows(url, actor, 1)
        _open_account_page(driver)
        assert _toggle_on(driver), "sign-out cleared the install's opt-in"
    finally:
        driver.teardown()


# ── The Apple leg ────────────────────────────────────────────────────────────
#
# Apple registers through APNs, and neither target can complete a registration
# yet: the `aps-environment` entitlement is an off-machine gate (`common.md`
# § Push Notifications → *Implementation status today*). Until it opens, the
# SPECIFIED behaviour is the failure half of `settings.md` § Push notifications
# — the toggle settles back off and the inline line renders — so that, not a
# landed row, is what this leg witnesses. When the gate opens this test goes
# red on its first assertion after the click, which is the cue to give Apple the
# three journeys above.


def _checked(driver) -> bool:
    return driver.get_attr(TOGGLE, "checked") == "true"


# No `feature` marker: this is not a witness of `notifications.md` outcome 9
# (off stays off; push follows whoever is signed in) — nothing can be switched
# on here to be switched off. It witnesses the control's failure half only.
@pytest.mark.macos
@pytest.mark.ios
def test_an_apple_install_that_cannot_register_stays_off_and_says_why(
    logged_in_app, nest_instance, test_user
):
    driver = logged_in_app
    url = nest_instance["url"]
    actor = test_user["actor_id_hex"]

    driver.set_state(GENERAL_PAGE_NAV)
    driver.wait_for(SECTION, timeout=15)
    assert not _checked(driver), "a fresh install is never opted in"
    assert _rows(url, actor) == [], "a launch registered a row nobody asked for"

    if driver.is_ios():
        # iOS answers the click with the OS notification prompt — a SpringBoard
        # alert no in-process driver can reach — so the enable attempt cannot
        # be driven past it here. What is witnessed on iOS is the control and
        # its off state; the settle-back-off line is the macOS half below (one
        # shared FaunaKit view and manager) and `PushManager`'s unit tests.
        return

    # macOS (the bare e2e binary has no notification centre at all): the enable
    # attempt fails, the toggle settles back off and the line says why. The
    # line is the causal anchor (convention 14) — it renders only once the
    # attempt resolved, so the off-state and no-row reads below are of the
    # settled state, not of a toggle that has not answered yet.
    driver.click(TOGGLE)
    driver.wait_for(ERROR, timeout=15)
    assert driver.get_text(ERROR), "the failure line rendered empty"
    assert not _checked(driver), _diagnose(driver)
    assert _rows(url, actor) == [], "a failed enable left a row behind"

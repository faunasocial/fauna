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
agent posts while the app is closed — `common.md` § Transports) and windows
follows on the same three journeys, each a shared body with one leg per app;
the Apple targets (APNs) follow with the one journey their entitlement gate
allows — the toggle that cannot register settling back off. Every mutation is
the app's own toggle, switch and sign-out (convention 8); the persisted truth
is read back through the nest's test-hooks surface
(`GET /api/v1/test/push/subscriptions`), the way the admin toggle tests read
theirs — never the app's word for it.

Latency-independence (convention 14): each "no row" assertion follows a causal
anchor in the same app — a row this session's own toggle wrote and then
removed, or the toggle reading the stored bit off after a relaunch, which is
the very value the launch's re-arm reads. The toggle's own reads wait for the
rendered state (`_wait_toggle`, `_settled_toggle`): the windows page paints the
stored bit after its load, and a toggle's reply can trail the nest's row.
"""
from __future__ import annotations

import json
import urllib.parse
import urllib.request

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

# The platform marker is per test: the tui and windows journeys launch their
# app themselves, the apple leg at the bottom rides `logged_in_app`.
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


def _toggle_state(driver) -> str | None:
    try:
        return driver.get_attr(TOGGLE, "state")
    except Exception:  # noqa: BLE001 - not rendered yet reads as no state
        return None


def _wait_toggle(driver, on: bool) -> None:
    """Wait until the toggle renders ``on`` — the stored bit after a toggle's
    reply, which can trail the row the nest already holds."""
    want = "on" if on else "off"
    seen: list[str | None] = []

    def rendered():
        seen.append(_toggle_state(driver))
        return seen[-1] == want

    wait_until(
        rendered,
        UI_SETTLE_S,
        diagnose=lambda: f"toggle state {seen[-1] if seen else None!r}, want {want!r}; {_diagnose(driver)}",
    )


def _settled_toggle(driver) -> bool:
    """The toggle once the page has painted it from the stored bit (``on`` or
    ``off`` — windows leaves the state unset until its load paints it)."""
    seen: list[str | None] = []

    def settled():
        seen.append(_toggle_state(driver))
        return seen[-1] in ("on", "off")

    wait_until(
        settled,
        UI_SETTLE_S,
        diagnose=lambda: f"toggle state {seen[-1] if seen else None!r}; {_diagnose(driver)}",
    )
    return seen[-1] == "on"


def _click_switcher_row(driver, index: int, rows: int = 2) -> None:
    """Switch identity through the switcher's own row, once it lists every
    account (windows fills the list after the page loads)."""
    driver.wait_for(SWITCHER_LIST, timeout=15)
    seen: list[int] = []

    def listed():
        seen.append(driver.count(SWITCHER_ITEM))
        return seen[-1] >= rows

    wait_until(listed, UI_SETTLE_S, diagnose=lambda: f"{SWITCHER_ITEM} count={seen[-1] if seen else None}")
    driver.click(SWITCHER_ITEM, index=index)


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


def _launch(app, app_path, nest_instance, request, tmp_path, seed, keyring_app):
    """Launch ``app`` over a store and an install base pinned under
    ``tmp_path``, so a relaunch reads the same install (the intent bit is
    install-scoped: tui's config base, windows' data dir — which the windows
    driver never deletes when the test supplies it)."""
    config = {
        "app_path": app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "credential_dir": str(tmp_path / "creds"),
        "keyring_app": keyring_app,
        "environment": _seeded_environment(request, nest_instance),
    }
    if app == "windows":
        config["data_dir"] = str(tmp_path / "data")
    else:
        config["xdg_base"] = str(tmp_path / "xdg")
    driver = create_driver(app)
    driver.launch(config)
    _wait_signed_in(driver)
    return driver, config


def _wait_signed_in(driver, actor: str | None = None, timeout: float = 30) -> None:
    """Wait for an authenticated session — as ``actor`` when named, the causal
    anchor after a switch (both apps report the incoming identity's id only
    once its session is the one in front)."""

    def signed_in(s):
        session = s.get("session", {})
        if not session.get("authenticated"):
            return False
        return actor is None or session.get("actor_id") == actor

    driver.wait_for_state(signed_in, timeout=timeout)


# ── Journey 1: off stays off across a relaunch ──────────────────────────────


@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_push_turned_off_stays_off_across_a_relaunch(
    nest_instance, tui_app_path, request, tmp_path
):
    _off_stays_off_across_a_relaunch("tui", tui_app_path, nest_instance, request, tmp_path)


@pytest.mark.windows
@pytest.mark.feature("notifications")
def test_windows_push_turned_off_stays_off_across_a_relaunch(
    nest_instance, windows_app_path, request, tmp_path
):
    _off_stays_off_across_a_relaunch(
        "windows", windows_app_path, nest_instance, request, tmp_path
    )


def _off_stays_off_across_a_relaunch(app, app_path, nest_instance, request, tmp_path):
    url = nest_instance["url"]
    actor, secret = _register_user(nest_instance)
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": url,
          "device_id": "push-off-stays-off", "handle": "pushuser"}],
        active=actor,
    )
    driver, config = _launch(
        app, app_path, nest_instance, request, tmp_path, seed, f"push-off-stays-off-{app}"
    )
    try:
        _open_account_page(driver)
        assert not _settled_toggle(driver), "a fresh install is never opted in"

        # On → this install's ws-device row exists.
        driver.click(TOGGLE)
        rows = _wait_rows(url, actor, 1)
        assert rows[0]["transport"] == "ws-device", rows
        _wait_toggle(driver, True)

        # Off → the row is gone and the toggle reads off.
        driver.click(TOGGLE)
        _wait_rows(url, actor, 0)
        _wait_toggle(driver, False)

        # Relaunch the SAME install: off stays off — the launch's re-arm reads the
        # bit the toggle renders, and must not restore the row.
        assert driver.preserve_state_across_relaunch()
        driver.teardown()
        relaunch = dict(config)
        relaunch.pop("seed_credentials", None)
        driver.launch(relaunch)
        _wait_signed_in(driver)
        _open_account_page(driver)
        assert not _settled_toggle(driver), "the relaunch must not read the install as opted in"
        assert _rows(url, actor) == [], "a re-arm opted a device in that the user turned off"
    finally:
        driver.teardown()


# ── Journey 2: push follows whoever is signed in ────────────────────────────


@pytest.mark.tui
@pytest.mark.feature("notifications")
@pytest.mark.feature("multiple-accounts")
def test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting(
    nest_instance, tui_app_path, request, tmp_path
):
    _follows_whoever_is_signed_in("tui", tui_app_path, nest_instance, request, tmp_path)


@pytest.mark.windows
@pytest.mark.feature("notifications")
@pytest.mark.feature("multiple-accounts")
def test_windows_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting(
    nest_instance, windows_app_path, request, tmp_path
):
    _follows_whoever_is_signed_in(
        "windows", windows_app_path, nest_instance, request, tmp_path
    )


def _follows_whoever_is_signed_in(app, app_path, nest_instance, request, tmp_path):
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
        app, app_path, nest_instance, request, tmp_path, seed, f"push-follows-{app}"
    )
    try:
        # Opt in as the first identity.
        _open_account_page(driver)
        assert not _settled_toggle(driver), "a fresh install is never opted in"
        driver.click(TOGGLE)
        _wait_rows(url, first, 1)
        _wait_toggle(driver, True)

        # Switch to the second (switcher row 1): the outgoing identity's row is
        # dropped, the incoming one is re-armed with NO Settings visit, and the
        # toggle — this device's setting — is still on (multiple-accounts 11).
        _click_switcher_row(driver, 1)
        _wait_rows(url, second, 1)
        _wait_rows(url, first, 0)
        _wait_signed_in(driver, actor=second, timeout=45)
        _open_account_page(driver)
        assert _settled_toggle(driver), "switching identity turned this device's push off"

        # Off as the second, then switch back: off stays off for the first too.
        driver.click(TOGGLE)
        _wait_rows(url, second, 0)
        _wait_toggle(driver, False)
        _click_switcher_row(driver, 0)
        _wait_signed_in(driver, actor=first, timeout=45)
        _open_account_page(driver)
        assert not _settled_toggle(driver), "switching identity turned this device's push on"
        assert _rows(url, first) == [], "a switch opted the incoming identity in"
    finally:
        driver.teardown()


# ── Journey 3: sign-out drops the row, the next sign-in re-arms ─────────────


@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_sign_out_drops_the_row_and_the_next_sign_in_rearms(
    nest_instance, tui_app_path, request, tmp_path
):
    _sign_out_drops_and_sign_in_rearms("tui", tui_app_path, nest_instance, request, tmp_path)


@pytest.mark.windows
@pytest.mark.feature("notifications")
def test_windows_sign_out_drops_the_row_and_the_next_sign_in_rearms(
    nest_instance, windows_app_path, request, tmp_path
):
    _sign_out_drops_and_sign_in_rearms(
        "windows", windows_app_path, nest_instance, request, tmp_path
    )


def _sign_out_drops_and_sign_in_rearms(app, app_path, nest_instance, request, tmp_path):
    from actions.settings import SettingsActions

    url = nest_instance["url"]
    actor, secret = _register_user(nest_instance)
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": url,
          "device_id": "push-sign-out", "handle": "signout"}],
        active=actor,
    )
    driver, config = _launch(
        app, app_path, nest_instance, request, tmp_path, seed, f"push-sign-out-{app}"
    )
    try:
        _open_account_page(driver)
        assert not _settled_toggle(driver), "a fresh install is never opted in"
        driver.click(TOGGLE)
        _wait_rows(url, actor, 1)
        _wait_toggle(driver, True)

        # Sign-out drops the leaving identity's row…
        SettingsActions(driver).sign_out()
        _wait_rows(url, actor, 0)

        # …and keeps this install's opt-in: the next sign-in on the same install
        # re-arms with no Settings visit and no prompt.
        assert driver.preserve_state_across_relaunch()
        driver.teardown()
        driver.launch(config)  # re-seeds the same identity over the same install
        _wait_signed_in(driver)
        _wait_rows(url, actor, 1)
        _open_account_page(driver)
        assert _settled_toggle(driver), "sign-out cleared the install's opt-in"
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

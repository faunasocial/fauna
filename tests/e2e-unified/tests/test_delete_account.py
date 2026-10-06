"""Real account deletion — the pending-action gate PLUS its completion.

`settings-delete-confirm-field`/`settings-delete-account-button` are built and
were gate-tested only at the unit level (`apps/fauna-tui/src/settings/mod.rs`):
clicking Delete after typing DELETE dispatches `Action::DeleteAccount` ->
`fauna.account.delete`, which only *schedules* a 14-day pending action
(`tests/e2e-unified/tests/api/test_onboarding.py`'s
`test_alice_self_service_onboarding` Step 10 asserts exactly that — pending
status, account still active immediately after). No test carried it through to
actual removal. This module closes that gap: type DELETE through the real UI,
force the pending action due via the nest's `run_due` test hook (precedent:
`test_atproto_firehose_post.py`'s handle-rename leg), and assert the account is
actually gone (`bins/fauna-nest/src/pending_actions.rs::finalize_user_deletion`
drops the `users` row, so its handle stops resolving).

A DEDICATED nest is required (`delete_account_nest`, not the shared
`nest_instance`) — see that fixture's docstring in `conftest.py`: `run_due`
force-executes EVERY still-pending action nest-wide, not just this test's own.
"""
from __future__ import annotations

import pytest

from clients.ws_rpc_anon_client import RpcCallError
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3]


@pytest.mark.feature("account")
def test_delete_account_via_confirm_field(delete_account_app, delete_account_nest):
    app = delete_account_app
    user = delete_account_nest["user"]
    port = delete_account_nest["port"]

    app.settings.navigate()
    app.settings._navigate_subpage("account")

    # (a) the button stays disabled before anything is typed.
    assert not app.settings.is_delete_account_enabled(), (
        "settings-delete-account-button must start disabled — typing DELETE "
        "into settings-delete-confirm-field is the whole point of the "
        "type-to-confirm gate"
    )

    # (b) typing the exact string DELETE enables it.
    app.settings.type_delete_confirm("DELETE")
    assert app.settings.is_delete_account_enabled(), (
        "settings-delete-account-button must enable once the confirm field "
        "reads exactly 'DELETE'"
    )

    # Precondition: the account is still live before we touch it.
    resolved = ws_api.actor_by_handle(port, user["handle"])
    assert resolved["actor_id"] == user["actor_id_hex"]

    # (c) confirming queues the 14-day pending action — NOT immediate deletion.
    app.settings.confirm_delete_account()
    assert not app.has_error(), (
        f"confirming account deletion must not surface an error: {app.error_text()}"
    )

    resolved = ws_api.actor_by_handle(port, user["handle"])
    assert resolved["actor_id"] == user["actor_id_hex"], (
        "the account must still resolve immediately after confirming — "
        "fauna.account.delete only SCHEDULES a 14-day pending action, it does "
        "not delete on the spot"
    )

    # (d) …and the APP stays where it was: signed in, on the Account page —
    # no sign-out, no credential/store erase, no navigation (settings.md
    # § Where logic lives → Account deletion, ruled 2026-08-26). The
    # pending-actions row is the receipt and its cancel button the way back;
    # an app that tore the session down here would strand the user outside
    # the 14-day cancel window with only their exported identity secret as
    # the way back in.
    # A positive read of a button at the foot of a long page, right after the
    # pending-actions receipt row lands above it: windows' `is_visible` reads UIA
    # `IsOffscreen`, so a button one scroll away must be brought into view first.
    assert app.driver.is_visible_scrolled("settings-delete-account-button"), (
        "after confirming, the app must stay on the Account page, signed in "
        "— tearing the session down at confirm makes the cancellable pending "
        "action effectively irreversible (settings.md § User actions)"
    )

    # Force the pending action due and run it through the REAL executor
    # (pending_actions_test_hook.rs), not a test-only shortcut around it.
    from conftest import _bridge_admin_post

    ran = _bridge_admin_post(
        delete_account_nest["url"],
        delete_account_nest["admin"]["token"],
        "/api/v1/test/pending_actions/run_due",
        {},
    )
    assert ran.get("executed", 0) >= 1, f"the account deletion never applied: {ran}"

    # The account is actually gone: its handle no longer resolves.
    with pytest.raises(RpcCallError) as excinfo:
        ws_api.actor_by_handle(port, user["handle"])
    assert excinfo.value.code == "fauna.actor.not_found", (
        f"expected fauna.actor.not_found once the deletion pending action "
        f"executed, got {excinfo.value.code!r}"
    )


@pytest.mark.tui
@pytest.mark.feature("account")
def test_tui_an_account_the_nest_stops_accepting_erases_nothing_on_the_device(
    delete_account_nest, tui_app_path, tmp_path, request
):
    """An account the nest stops accepting does not erase what is on this
    device; only the user's own sign-out does that (`settings.md` § Where logic
    lives → Account deletion: the refusal is `fauna.auth.not_registered`,
    opaque by design — no deleted-vs-suspended oracle — so the app must not
    auto-erase on it; a suspension may be lifted).

    The account is deleted out from under the app, as from another device:
    the deletion is scheduled over the wire and forced due through the nest's
    REAL executor (`/api/v1/test/pending_actions/run_due`, the dedicated
    nest's reason to exist). The app then relaunches onto the pinned store —
    its next sign-in is the refused one — and must (a) land somewhere that is
    not the signed-in app, so the refusal is visible, and (b) still hold the
    identity's secret, its registry row and its scoped `mls_state.db`.

    The mutation that matters here is not a user action (the nest does it), so
    driving it through the API is the scenario, not a stand-in (convention 8).
    Every wait is on state (point 14)."""
    _an_account_the_nest_stops_accepting_erases_nothing(
        "tui", tui_app_path, "fauna-tui", delete_account_nest, tmp_path, request
    )


@pytest.mark.linux
@pytest.mark.feature("account")
def test_linux_an_account_the_nest_stops_accepting_erases_nothing_on_the_device(
    delete_account_nest, linux_app_path, tmp_path, request
):
    """linux's leg of the tui journey above — the same contract over linux's
    file-backed store and its flat `fauna` scope base."""
    _an_account_the_nest_stops_accepting_erases_nothing(
        "linux", linux_app_path, "fauna", delete_account_nest, tmp_path, request
    )


@pytest.mark.windows
@pytest.mark.feature("account")
def test_windows_an_account_the_nest_stops_accepting_erases_nothing_on_the_device(
    delete_account_nest, windows_app_path, tmp_path, request
):
    """windows' leg of the tui journey above — the same contract over windows'
    file-backed store (`{credential_dir}/{keyring_app}.json`) and its
    `WindowsScopeStore` flat base (`AccountStateDir.FlatBase`, where the
    account's scoped `mls.db` rests). The relaunch keeps both halves because
    the test supplies the credential and data dirs, which the driver never
    empties or deletes (`drivers/windows.py` `_owns_data_dir`)."""
    _an_account_the_nest_stops_accepting_erases_nothing(
        "windows", windows_app_path, None, delete_account_nest, tmp_path, request
    )


@pytest.fixture
def apple_app(request):
    """The apple app an item drives — INDIRECT on purpose, so `--app macos` /
    `--app ios` deselect the other leg (a direct parametrize is a pseudo-fixture
    `conftest._parametrized_clients` ignores; `test_account_switcher_apple.py`'s
    `app_name` says why)."""
    return request.param


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("account")
@pytest.mark.parametrize("apple_app", ["macos", "ios"], indirect=True)
def test_apple_an_account_the_nest_stops_accepting_erases_nothing_on_the_device(
    delete_account_nest, apple_app, tmp_path, request
):
    """The apple legs (macOS + iOS) of the tui journey above — the same
    contract over apple's E2E keychain file (`{cred_dir}/keychain.json`) and
    its `AppleScopeStore` flat base (`<Application Support>/Fauna`, where the
    account's scoped `mls.db` rests). The relaunch pins both halves of the
    store (`preserve_state_across_relaunch`: keychain + relocated home on
    macOS, keychain + simulator container on iOS)."""
    if apple_app == "ios":
        setup = request.getfixturevalue("ios_setup")
        launch = {"app_path": setup["app_path"], "udid": setup["udid"]}
    else:
        launch = {"app_path": request.getfixturevalue("macos_app_path")}
    _an_account_the_nest_stops_accepting_erases_nothing(
        apple_app, None, None, delete_account_nest, tmp_path, request,
        apple_launch=launch,
    )


def _an_account_the_nest_stops_accepting_erases_nothing(
    app, app_path, flat_base, delete_account_nest, tmp_path, request,
    *, apple_launch=None,
):
    """The shared body: `app` is the driver name, `flat_base` its
    `XdgScopeStore` flat base (tui `fauna-tui`, linux `fauna`; `None` for
    windows, whose scoped state is under `WindowsScopeStore`). The apple
    drivers pass `apple_launch` (their own launch keys) instead: their store
    is the host-side keychain file and their scoped state is under
    `AppleScopeStore`."""
    import json
    import os

    from common import build_registry_seed
    from common.auth import _authed_call
    from common.scope_store import XdgScopeStore, attach_scope_store
    from conftest import _bridge_admin_post, _seeded_environment
    from drivers import create_driver
    from helpers.budgets import APP_RELAUNCH_S
    from helpers.waiting import wait_until

    nest = delete_account_nest
    user = nest["user"]
    actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": bytes(user["signing_key"]).hex(),
          "nest_url": nest["url"], "device_id": "nest-refusal", "handle": user["handle"]}],
        active=actor,
    )
    driver = create_driver(app)
    config = {
        "url": nest["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest),
    }
    if apple_launch is not None:
        config.update(apple_launch)
    else:
        config.update({
            "app_path": app_path,
            "credential_dir": str(tmp_path / "creds"),
            "keyring_app": f"nest-refusal-{app}",
        })
        if app == "windows":
            config["data_dir"] = str(tmp_path / "data")
        else:
            config["xdg_base"] = str(tmp_path / "xdg")
    driver.launch(config)

    def store_path():
        if apple_launch is not None:
            return os.path.join(driver._cred_dir, "keychain.json")
        return os.path.join(
            driver._resolved_credential_dir, f"{driver._resolved_keyring_app}.json"
        )

    def store_slot(key):
        try:
            with open(store_path()) as f:
                return json.load(f).get(key)
        except (OSError, ValueError):
            return None

    try:
        # (1) A working session, so the identity owns real on-device state.
        wait_until(
            lambda: (driver.get_state() or {}).get("session", {}).get("actor_id") == actor,
            APP_RELAUNCH_S,
            diagnose=lambda: f"session={(driver.get_state() or {}).get('session')!r}",
        )
        if apple_launch is not None or app == "windows":
            scoped_db = attach_scope_store(app, driver).scope_dirs(actor)[0] / "mls.db"
        else:
            scoped_db = XdgScopeStore(app, driver.config_home, flat_base).scope_dirs(
                actor
            )[0] / "mls_state.db"
        wait_until(scoped_db.exists, 30, diagnose=lambda: f"{scoped_db} never appeared")
        secret_before = store_slot(f"fauna/{actor}/secret")
        assert secret_before, "precondition: the identity's secret is on the device"

        # (2) The nest stops accepting the account.
        _authed_call(nest["url"], user["signing_key"], "fauna.account.delete", {})
        ran = _bridge_admin_post(
            nest["url"], nest["admin"]["token"], "/api/v1/test/pending_actions/run_due", {}
        )
        assert ran.get("executed", 0) >= 1, f"the deletion never applied: {ran}"

        # (3) Relaunch onto the SAME store: the next sign-in is the refused one.
        assert driver.preserve_state_across_relaunch(), (
            f"the {app} driver must be able to pin its file store across a relaunch"
        )
        driver.teardown()
        relaunch = dict(driver._launch_config)
        relaunch.pop("seed_credentials", None)
        driver.launch(relaunch)

        # (a) The refusal is visible: the app does not come up signed in as the
        # refused identity; it lands on the launch/onboarding surface.
        def refused_surface():
            s = driver.get_state() or {}
            nav = [v.get("view") for v in s.get("nav", {}).get("stack", [])]
            return not s.get("session", {}).get("authenticated") and "welcome" in nav

        def describe_surface():
            s = driver.get_state() or {}
            session = s.get("session", {})
            return (
                f"nav={s.get('nav')!r}; authenticated={session.get('authenticated')!r}; "
                f"messages={s.get('messages')!r:.300}"
            )

        wait_until(refused_surface, APP_RELAUNCH_S, diagnose=describe_surface)

        # (b) …and nothing on the device was erased.
        assert store_slot(f"fauna/{actor}/secret") == secret_before, (
            "the nest refusing the account must not erase its secret from this device"
        )
        index = store_slot("fauna/index")
        assert index and actor in str(index), (
            f"the refused identity must still be registered on this device; index={index!r}"
        )
        # Re-resolve the scope against the RELAUNCHED process: an iOS in-place
        # reinstall keeps the data container but may hand it a new UUID, so the
        # first launch's path can name a directory the data has moved out of.
        survived_db = (
            attach_scope_store(app, driver).scope_dirs(actor)[0] / "mls.db"
            if apple_launch is not None else scoped_db
        )
        assert survived_db.exists(), (
            f"the refused identity's on-device data must survive; {survived_db} is gone "
            f"(first launch's path {scoped_db}: exists={scoped_db.exists()})"
        )
    finally:
        driver.teardown()



@pytest.mark.web
@pytest.mark.feature("account")
def test_web_an_account_the_nest_stops_accepting_erases_nothing_on_the_device(
    delete_account_nest, delete_account_spa_url
):
    """web's leg of the tui journey above: an account the nest stops accepting
    does not erase what is on this device (`settings.md` § Where logic lives →
    Account deletion). Web's on-device state is the registry in
    `localStorage` — the identity's `fauna/<actor>/secret` slot and its
    `fauna/index` row — so those are what must survive. The page reload is
    web's relaunch; its launch flow re-runs from the registry and meets the
    refused sign-in."""
    import json

    from common import build_registry_seed
    from common.auth import _authed_call
    from conftest import _bridge_admin_post
    from drivers import create_driver
    from helpers.budgets import APP_RELAUNCH_S
    from helpers.waiting import wait_until

    nest = delete_account_nest
    spa = delete_account_spa_url
    user = nest["user"]
    actor = user["actor_id_hex"]
    secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": spa,
          "device_id": "nest-refusal", "handle": user["handle"]}],
        active=actor,
    )
    driver = create_driver("web")
    driver.launch({"url": spa + "/app/"})

    def ls(key):
        return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")

    def session():
        return (driver.get_state() or {}).get("session", {})

    try:
        # (1) Signed in as the identity, from the registry this device holds.
        driver.eval_js(";".join(
            f"localStorage.setItem({json.dumps(k)},{json.dumps(v)})" for k, v in seed.items()
        ))
        driver.hard_reload()
        wait_until(
            lambda: session().get("actor_id") == actor and session().get("authenticated"),
            APP_RELAUNCH_S,
            diagnose=lambda: f"session={session()!r}",
        )
        assert ls(f"fauna/{actor}/secret") == secret, "precondition: the secret is on the device"

        # (2) The nest stops accepting the account.
        _authed_call(nest["url"], user["signing_key"], "fauna.account.delete", {})
        ran = _bridge_admin_post(
            nest["url"], nest["admin"]["token"], "/api/v1/test/pending_actions/run_due", {}
        )
        assert ran.get("executed", 0) >= 1, f"the deletion never applied: {ran}"

        # (3) Relaunch: the next sign-in is the refused one, and it is visible —
        # the app lands on onboarding, not signed in.
        driver.hard_reload()

        def refused_surface():
            path = driver.eval_js("location.pathname") or ""
            return not session().get("authenticated") and "onboarding" in path

        wait_until(
            refused_surface,
            APP_RELAUNCH_S,
            diagnose=lambda: (
                f"path={driver.eval_js('location.pathname')!r} session={session()!r}"
            ),
        )

        # (b) …and nothing on the device was erased.
        assert ls(f"fauna/{actor}/secret") == secret, (
            "the nest refusing the account must not erase its secret from this device"
        )
        index = ls("fauna/index")
        assert index and actor in index, (
            f"the refused identity must still be registered on this device; index={index!r}"
        )
    finally:
        driver.teardown()

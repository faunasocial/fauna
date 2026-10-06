"""tier_3 e2e: a successor's inherited profile stays editable without a recorded link.

Goal doc: ``docs/goal/ui/profile.md`` § Persistence → *After an identity
succession, the successor RE-PUBLISHES; it does not rewrite*. A succession moves
the profile row's ownership to the successor but leaves its signed bytes alone,
so the row the successor edits still names the predecessor. A writer admits
such a base only when the device's account registry records the succession. The
section names two ways a device ends up without that record, and each gets a
journey here:

1. **A device that never held the predecessor's row** — any second device,
   since adding an account carries no link. It must LEARN the link from the
   landed statement (the public succession path, accepted only as a chain that
   ends at the signer's own ``new_sig``) and then save the edit.
2. **The user removed the retired account** on a device that did hold the link.
   The link lives on the successor's own registry row as well, so the removal
   cannot strand the profile.

Either failure leaves the inherited profile with no writer on that device — and
no writer anywhere once every device is in that state, which is a
client-unrecoverable state and therefore a bug (``nest/common.md`` § Client-state
recoverability).

**Arranged outside the app, on purpose (convention 8's carve-out).** Each
journey needs a stored row that is still PREDECESSOR-signed when the successor
edits it. The real succession ceremony cannot provide that on tui: its closing
act mints the successor's kit on the first session, and the kit's profile mirror
re-signs the row under the successor, so every later edit would read a
self-signed base and prove nothing. So the predecessor's profile, its recovery
kit and the succession are all minted through the shared-Rust fixture
(``helpers/succession.py``), and each test asserts the row is still
predecessor-signed before the app touches it. The seats' registries are seeded
the way ``test_account_switcher_tui.py`` seeds them. Everything the user does is
UI: opening the edit form, saving it, removing the retired account.

**Latency-independent (convention 14).** The edit form is shown at once and
seeded when its base arrives. The base load is where a linkless device proves
and records the link (``fauna_client_recovery::ceremony::load_profile_edit_base``),
so "the form shows the inherited name" is the stable gate the test waits on
before typing. Nothing waits on the background sign-in hop, which the save no
longer depends on. Outcomes are read off the stored bytes and the registry file,
never off a timing window.

**What makes each journey go red.** Arm 1 fails when nothing proves the link:
the sign-in hop and the edit form's base load both route through
``fauna_client_profile::fetch_own_profile_base``. Arm 2 fails when
``AccountRegistry::predecessors_of`` stops reading ``succeeded_from``, the
successor-side record that survives the removal.

**Which apps.** Both arms run on every app whose seat can seed a registry and
read it back — tui, linux and windows over the file-backed ``CredentialStore``,
macos and ios over the e2e build's file-backed keychain (``keychain.json`` under
the per-launch credential dir, which ``InProcessAgentDriver`` seeds from
``seed_credentials`` before the process starts — ``KeychainStore.e2eFileURL``),
web over the origin's ``localStorage`` (a seeded registry and a reload, the boot
``test_succession_second_tab_web.py`` uses). Each loads its edit base through
the same shared read-prove-record, and arm 2's removal goes through each app's
own switcher, so each arm is one journey with a per-app seat.

tier_3: a real ``fauna-nest`` binary; the tui, linux, web, windows, macos and
ios drivers.
"""
from __future__ import annotations

import json
import os
import secrets
import uuid

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver
from drivers.inprocess_agent import InProcessAgentDriver
from drivers.tui import TuiDriver
from drivers.windows import WindowsBridgeDriver
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.succession import (
    publish_signed_profile,
    register_recovery_kit,
    stored_profile,
    succeed_identity,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

# tests/e2e-unified/ui.yaml § settings (the account switcher).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
REMOVE_BUTTON = "account-remove-button"
# tests/e2e-unified/ui.yaml § profile (the edit form's name field).
EDIT_DISPLAY_NAME = "profile-edit-display-name"

ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _reader(nest_instance) -> dict:
    """The nest's own admin identity, as a session to read rows over.

    ``fauna.profile.get`` serves any actor's row, and the successor has no
    Python-side session of its own (its only session is the app's).
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    return {
        "reader_actor_id_hex": bytes(admin_sk.verify_key).hex(),
        "reader_seed_hex": bytes(admin_sk).hex(),
    }


def _succeeded_account(nest_instance, tag: str) -> dict:
    """A fresh account whose predecessor signed the profile it now owns.

    Publishes the predecessor's profile, registers its recovery kit and runs the
    succession, all through the fixture. Returns both identities, and the
    predecessor's display fields so a test can check they travelled.
    """
    url = nest_instance["url"]
    predecessor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    predecessor_id = predecessor["actor_id_hex"]
    predecessor_seed = bytes(predecessor["signing_key"]).hex()
    name, bio = f"Predecessor {tag}", f"A bio that travels with the account {tag}"

    publish_signed_profile(
        url,
        actor_id_hex=predecessor_id,
        identity_seed_hex=predecessor_seed,
        display_name=name,
        bio=bio,
    )
    kit_secret = register_recovery_kit(
        url, actor_id_hex=predecessor_id, identity_seed_hex=predecessor_seed
    )
    successor_seed = secrets.token_bytes(32).hex()
    successor_id = succeed_identity(
        url,
        old_actor_id_hex=predecessor_id,
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=predecessor_seed,
    )
    return {
        "predecessor_id": predecessor_id,
        "predecessor_seed": predecessor_seed,
        "successor_id": successor_id,
        "successor_seed": successor_seed,
        "name": name,
        "bio": bio,
    }


def _assert_still_predecessor_signed(nest_instance, account: dict, when: str) -> None:
    """The precondition every assertion below leans on: the successor owns a row
    whose bytes the PREDECESSOR signed. If something had already re-signed it,
    the edit would read a self-signed base and the journey would prove nothing."""
    row = stored_profile(
        nest_instance["url"],
        owner_actor_id_hex=account["successor_id"],
        **_reader(nest_instance),
    )
    assert (row.actor_id, row.origin) == (account["predecessor_id"], "direct"), (
        f"{when}, the successor's row must still be the predecessor's own signed "
        "bytes, or the edit below never exercises the inherited-base path. "
        f"predecessor={account['predecessor_id']} successor="
        f"{account['successor_id']} row={row!r}"
    )
    assert row.display_name == account["name"], (
        f"the arranged row does not carry the predecessor's fields: {row!r}"
    )


def _apps() -> list[str]:
    available = get_available_apps()
    return [a for a in ("tui", "linux", "web", "windows", "macos", "ios") if a in available]


@pytest.fixture(params=_apps())
def linkless_app(request):
    """The app under both arms; its id lands in the test name, which conftest's
    ``--app`` filter reads."""
    return request.param


def _launch_seat(request, nest_instance, app: str, seed: dict):
    driver = create_driver(app)
    launch_config = {
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    }
    if app == "ios":
        # No bare `ios_app_path` fixture: `ios_setup` carries the simctl `udid`
        # `drivers/ios.py`'s `launch()` needs beside the app path.
        ios_setup = request.getfixturevalue("ios_setup")
        launch_config["app_path"] = ios_setup["app_path"]
        launch_config["udid"] = ios_setup["udid"]
    else:
        launch_config["app_path"] = request.getfixturevalue(f"{app}_app_path")
    driver.launch(launch_config)
    return driver


def _wait_signed_in(driver, as_actor: str | None = None) -> None:
    driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated"))
        and (as_actor is None or s.get("session", {}).get("actor_id") == as_actor),
        timeout=APP_RELAUNCH_S,
    )


def _launch_registry_seat(request, nest_instance, app: str, rows: list[dict], active: str):
    """A seat whose account registry holds exactly ``rows`` (each without its
    ``nest_url``, which is per-app), signed in as ``active``."""
    if app != "web":
        seed = build_registry_seed(
            [{**row, "nest_url": nest_instance["url"]} for row in rows], active=active
        )
        driver = _launch_seat(request, nest_instance, app, seed)
        _wait_signed_in(driver, as_actor=active)
        return driver

    # Web has no pre-navigation seeding hook: seed the origin's store, then
    # reboot over it, so the SPA signs in from the registry alone.
    spa_url = request.getfixturevalue("spa_url")
    seed = build_registry_seed(
        [{**row, "nest_url": spa_url, "device_id": secrets.token_hex(32)} for row in rows],
        active=active,
    )
    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        driver.eval_js(
            ";".join(
                f"localStorage.setItem({json.dumps(k)},{json.dumps(v)})"
                for k, v in seed.items()
            )
        )
        driver.hard_reload()
        _wait_signed_in(driver, as_actor=active)
    except BaseException:
        driver.teardown()
        raise
    return driver


def _launch_linkless_seat(request, nest_instance, app: str, account: dict):
    """A fresh install that added the successor's account and nothing else: one
    registry row, no link, no predecessor material — signed in as the successor.
    """
    row = {
        "actor_id": account["successor_id"],
        "secret_hex": account["successor_seed"],
        "device_id": "linkless-successor",
    }
    return _launch_registry_seat(
        request, nest_instance, app, [row], active=account["successor_id"]
    )


def _credential_file(driver) -> str:
    """The seat's file-backed ``CredentialStore`` — where each driver's
    ``seed_credentials`` wrote the seed (``test_account_switcher_tui.py`` /
    ``test_account_switcher_linux.py`` / ``test_account_switcher_windows.py``
    read the same files; apple's is ``keychain.json``, the file
    ``test_account_switcher_apple.py`` seeds)."""
    if isinstance(driver, (TuiDriver, InProcessAgentDriver)):
        return os.path.join(
            driver._resolved_credential_dir, f"{driver._resolved_keyring_app}.json"
        )
    if isinstance(driver, WindowsBridgeDriver):
        return os.path.join(driver._cred_dir, f"{driver._keyring_app}.json")
    return os.path.join(driver._tmp_dir, "creds", f"fauna-e2e-agent-{driver._agent_port}.json")


def _registry_index(driver) -> dict:
    """The persisted ``AccountIndex`` the registry writes verbatim
    (``fauna/index``) — the file backend's on tui and linux, the origin's
    ``localStorage`` on web. ``{}`` while it is unreadable."""
    if driver.is_web():
        try:
            raw = driver.eval_js("localStorage.getItem('fauna/index')")
        except RuntimeError:
            return {}
        return json.loads(raw) if raw else {}
    cred_file = _credential_file(driver)
    try:
        with open(cred_file) as f:
            raw = json.load(f).get("fauna/index")
    except (OSError, ValueError):
        return {}
    if raw is None:
        return {}
    return json.loads(raw) if isinstance(raw, str) else raw


def _succeeded_from(driver, actor_id: str):
    for entry in _registry_index(driver).get("accounts", []):
        if entry.get("actor_id") == actor_id:
            return entry.get("succeeded_from", [])
    return None


def _save_a_display_name_edit(app, *, inherited_name: str, new_name: str) -> None:
    """Open the SELF edit form, wait for the inherited base, rename, save."""
    profile = app.profile
    profile.navigate()
    profile.open_edit_form()
    # The form is shown before its base arrives; the base load proves the link
    # a linkless device lacks. The seeded name is the stable sign that it ran.
    wait_until(
        lambda: app.driver.get_text(EDIT_DISPLAY_NAME) == inherited_name,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the edit form never showed the inherited profile's name, so its "
            f"base never loaded. field={app.driver.get_text(EDIT_DISPLAY_NAME)!r} "
            f"error={profile.error_text()!r}"
        ),
    )
    profile.set_display_name(new_name)
    profile.save()
    saved = profile.wait_for_save(timeout=RPC_ROUNDTRIP_S)
    assert saved and not profile.error_text(), (
        "saving an edit over the inherited profile was refused. A refusal naming "
        "the predecessor means the device admitted no link for it: arm 1 — the "
        "base load did not prove the landed succession; arm 2 — the link did not "
        f"survive the retired account's removal. error={profile.error_text()!r}"
    )


def _assert_republished_by_successor(nest_instance, account: dict, new_name: str) -> None:
    row = stored_profile(
        nest_instance["url"],
        owner_actor_id_hex=account["successor_id"],
        **_reader(nest_instance),
    )
    assert (row.actor_id, row.origin) == (account["successor_id"], "direct"), (
        "the saved row must be a new version signed by the successor's own key. "
        f"successor={account['successor_id']} row={row!r}"
    )
    assert row.display_name == new_name, f"the edit did not land: {row!r}"
    assert row.bio == account["bio"], (
        "the predecessor's other display fields must travel with the account "
        f"(profile.md admission rule 4); got {row!r}"
    )


@pytest.mark.feature("take-your-account-back")
def test_a_device_that_never_held_the_predecessor_saves_a_profile_edit(
    nest_instance, linkless_app, request
):
    """A second device holding only the successor learns the link and saves."""
    account = _succeeded_account(nest_instance, uuid.uuid4().hex[:8])
    _assert_still_predecessor_signed(nest_instance, account, "before the device signs in")

    driver = _launch_linkless_seat(request, nest_instance, linkless_app, account)
    try:
        new_name = f"Successor {uuid.uuid4().hex[:8]}"
        _save_a_display_name_edit(
            ActionLayer(driver), inherited_name=account["name"], new_name=new_name
        )
        _assert_republished_by_successor(nest_instance, account, new_name)

        # What admitted the base is recorded on the successor's own row, so the
        # next launch needs no lookup. A save cannot succeed without it, so this
        # holds the moment the save does.
        assert _succeeded_from(driver, account["successor_id"]) == [
            account["predecessor_id"]
        ], f"the proven link was not recorded: index={_registry_index(driver)!r}"
    finally:
        driver.teardown()


@pytest.mark.feature("take-your-account-back")
def test_a_profile_edit_still_saves_after_the_retired_account_is_removed(
    nest_instance, linkless_app, request
):
    """The device that holds the link removes the retired account, then edits."""
    account = _succeeded_account(nest_instance, uuid.uuid4().hex[:8])
    _assert_still_predecessor_signed(nest_instance, account, "before the device signs in")

    # The device that ran the succession, as `record_succession` leaves it: the
    # retired row carries `succeeded_by`, the successor's row `succeeded_from`.
    # Seeded rather than run through the ceremony, whose closing act would
    # re-sign the row (module docstring).
    driver = _launch_registry_seat(
        request,
        nest_instance,
        linkless_app,
        [
            {
                "actor_id": account["successor_id"],
                "secret_hex": account["successor_seed"],
                "device_id": "succeeded-device",
                "succeeded_from": [account["predecessor_id"]],
            },
            {
                "actor_id": account["predecessor_id"],
                "secret_hex": account["predecessor_seed"],
                "device_id": "succeeded-device-retired",
                "succeeded_by": account["successor_id"],
            },
        ],
        active=account["successor_id"],
    )
    try:
        # Remove the retired account through the switcher. It is the only
        # non-active row, so it carries the only remove button.
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_LIST, timeout=UI_SETTLE_S)
        wait_until(
            lambda: driver.count(SWITCHER_ITEM) == 2,
            UI_SETTLE_S,
            diagnose=lambda: f"switcher rows={driver.count(SWITCHER_ITEM)}",
        )
        driver.click(REMOVE_BUTTON)
        wait_until(
            lambda: [
                a.get("actor_id") for a in _registry_index(driver).get("accounts", [])
            ] == [account["successor_id"]],
            UI_SETTLE_S,
            diagnose=lambda: (
                "the retired account was not removed from the registry: "
                f"index={_registry_index(driver)!r}"
            ),
        )
        assert _succeeded_from(driver, account["successor_id"]) == [
            account["predecessor_id"]
        ], (
            "removing the retired row must leave the successor's own record of "
            f"the link: index={_registry_index(driver)!r}"
        )

        # Nothing on this device re-signed the row meanwhile, so the save below
        # still has to admit a predecessor's base.
        _assert_still_predecessor_signed(
            nest_instance, account, "after the retired account is removed"
        )

        new_name = f"Successor {uuid.uuid4().hex[:8]}"
        _save_a_display_name_edit(
            ActionLayer(driver), inherited_name=account["name"], new_name=new_name
        )
        _assert_republished_by_successor(nest_instance, account, new_name)
    finally:
        driver.teardown()

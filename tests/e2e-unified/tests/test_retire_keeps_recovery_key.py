"""Outcome 34 of the ``nest_retire`` page — retiring a box leaves its recovery
key (docs/goal/behavior/nest-retirement.md § Errors & edge cases, *Retired ≠
forgotten*).

Its own module, apart from ``test_nest_retire.py``, because its apps are the
ones whose launch-retry surface offers both ``launch-retire-button`` and
``launch-recover-button`` over a device's own custodied seed (tui, linux, web,
android, macos — ``test_box_recovery_two_nest.py`` T4 — and ios). The retire
page's module carries a windows marker, and markers fold, so a test that does
not yet speak for windows lives under markers of its own.

**On web the device is one tab** (``SurvivingSeat``): left open while the box
dies and reloaded for each later launch, over the same origin store. A browser
custodies the nest's recovery seed like every other app
(``box-recovery.md`` § Implementation status today), so the outcome is owed
there too.

**iOS reaches that surface through its gating case, not its common dead-box
relaunch.** A stored session enters optimistically and stays in its cached shell
when the nest is dead (``onboarding.md`` § App-launch routing, the iOS
entry-posture paragraph), so a dead box alone never shows ``launch_retry`` there,
and whether that should change is the owner's question. What this journey witnesses on iOS is the outcome: on
the surface where recovery is offered, retiring keeps the seed. Each relaunch
carries the stale pending-factory-reset slot that withholds the entry
(``helpers/launch_retry_seed.py``), which names a second, live nest because the
reconcile must clear it; the seed under test is still the one this device
custodied by claiming the box.
"""

from __future__ import annotations

import secrets
import sys
from pathlib import Path

import pytest

_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from actions import ActionLayer  # noqa: E402
from actions.retire import RetireActions  # noqa: E402
from common.cred_store import AppleFileCredStore  # noqa: E402
from common.nest import stop_nest  # noqa: E402
from conftest import _seeded_environment, get_available_apps  # noqa: E402
from helpers.app_launch import app_launch_config  # noqa: E402
from helpers.box_recovery_launch import (  # noqa: E402
    SurvivingSeat,
    app_path_for,
    claim_fresh_identity,
    rust_log,
    start_boxes,
)
from helpers.indexed_rows import diagnose_row, row_visible, wait_for_row  # noqa: E402
from helpers.launch_retry_seed import stale_reset_slot_fields  # noqa: E402

pytestmark = [
    pytest.mark.tier_2,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

RECOVER_BOX_ITEM = "recover-box-item"  # indexed; read via helpers.indexed_rows
HETZNER_TOKEN = "MOCK-hetzner-token"  # the stubbed cloud accepts any token

#: The apps whose launch-retry surface offers both ``launch-retire-button`` and
#: ``launch-recover-button`` over a device's own custodied seed — the retire
#: page's apps that also run the dead-saved-nest recovery journey
#: (``test_box_recovery_two_nest.py`` T4).
_RECOVERY_APPS = [
    c for c in ("tui", "linux", "web", "android", "macos", "ios")
    if c in get_available_apps()
]

#: The apps that enter optimistically off a stored session, so a dead saved nest
#: alone leaves them in their cached shell (the module docstring).
OPTIMISTIC_ENTRY_APPS = ("ios",)


@pytest.mark.parametrize("client", _RECOVERY_APPS)
@pytest.mark.feature("retire-a-nest")
def test_retire_keeps_the_nests_recovery_key(
    client, nest_mode, tmp_path, tmp_path_factory, request, fake_cloud
):
    """Outcome 34 (nest-retirement.md § Errors & edge cases, *Retired ≠
    forgotten*): retiring the box a device is saved against leaves the box's
    custodied deployment seed where it was, so recovery still offers it.

    A device claims a box and the custody leg rests the box's seed in the
    device's own account store; the box dies; the relaunched device lands on
    launch-retry, whose ``launch-recover-button`` the seed reveals. From that
    same surface the person retires the server — the box they were saved
    against, so the done state routes back to the launch flow — and the
    recovery entry still lists the box, then again from a cold relaunch, which
    reads only what the device kept. On web each later launch is a reload of
    the one tab (``SurvivingSeat``)."""
    browser = client == "web"
    (nest,), stop_boxes = start_boxes(
        request, nest_mode, tmp_path_factory, "retire-keeps-seed", browser=browser)
    seat = None
    try:
        secret_hex = secrets.token_hex(32)
        handle = f"admin{secrets.token_hex(3)}@127.0.0.1:{nest['port']}"
        if browser:
            config = {
                "url": f"{request.getfixturevalue('spa_url')}/app/",
                "environment": _seeded_environment(request, nest),
            }
        else:
            launch_keys = (
                app_launch_config(client, request) if client == "ios"
                else {"app_path": app_path_for(client)}
            )
            config = {
                **launch_keys,
                "xdg_base": str(tmp_path / "xdg"),
                "credential_dir": str(tmp_path / "creds"),
                "keyring_app": "fauna-e2e-retire-keeps-seed",
                "environment": {**_seeded_environment(request, nest),
                                "RUST_LOG": rust_log(client)},
            }
        if client == "android":
            # The relaunches are the same device: keep the store launch 1
            # custodied into (test_box_recovery_two_nest.py T4, the same pin).
            config["preserve_store"] = True
        if client in ("macos", "tui"):
            # The same device again: pin the HOME the account store hangs off.
            # Left to its default each launch is a separate install, and launch 2
            # would boot a device that never custodied the seed (T4, the same pin).
            # tui reads it on darwin alone; on linux `xdg_base` is already the pin.
            config["home"] = str(tmp_path / "home")

        def withhold_optimistic_entry() -> None:
            """Before a relaunch on an app that enters optimistically: the stale
            slot that holds it on the launch gate. The launch clears it, so each
            relaunch needs its own; the slot's nest is live and claimed because
            that is what makes the reconcile clear it, and it is never the box
            this device custodied (the module docstring)."""
            if client not in OPTIMISTIC_ENTRY_APPS:
                return
            claimed_url = request.getfixturevalue("nest_instance")["url"]
            AppleFileCredStore(client, config["credential_dir"]).inject_raw(
                stale_reset_slot_fields(secret_hex, claimed_url))

        def recovery_lists_the_box(app, why: str) -> None:
            app.driver.wait_for("launch-recover-button", timeout=60)
            app.click("launch-recover-button")
            wait_for_row(app.driver, RECOVER_BOX_ITEM, 0, timeout=30)
            assert row_visible(app.driver, RECOVER_BOX_ITEM, 0), (
                f"{why}: nest_recovery no longer lists the box — "
                + diagnose_row(app.driver, RECOVER_BOX_ITEM, 0)
            )

        seat = SurvivingSeat(client, nest, config)

        # -- launch 1: claim the box; its seed rests in this device's store ----
        driver = seat.launch()
        try:
            claim_fresh_identity(ActionLayer(driver), secret_hex, handle, nest["claim_code"])
            seat.await_custody(driver, secret_hex)
        finally:
            seat.leave()

        # -- the box dies; the cloud account still holds its server ------------
        stop_nest(nest, graceful=True)
        cloud = fake_cloud.hetzner_cloud
        cloud.seed_managed_server(server_id=401, name="lost-box", ipv4="127.0.0.1", ptr=None)

        # -- launch 2: retire the server from the launch-retry surface ---------
        withhold_optimistic_entry()
        driver = seat.relaunch()
        try:
            app = ActionLayer(driver)
            driver.wait_for("launch-recover-button", timeout=60)
            provider_urls = {"vps": fake_cloud.url_map()["vps"]}
            if browser:
                # A tab's override is one map, and setting it reloads the page:
                # keep the dead box's address in it, so the reloaded launch
                # still dials the box this device is saved against.
                provider_urls["nest"] = nest["url"]
            driver.set_provider_base_urls(provider_urls)
            retire = RetireActions(driver)
            retire.open_from_launch()
            retire.enter_token("hetzner", HETZNER_TOKEN)
            retire.select("lost-box")
            retire.begin_confirm()
            retire.type_name("lost-box")
            retire.confirm()
            retire.wait_done()
            assert retire.step_error(1) is None, f"the server step failed: {retire.step_error(1)!r}"
            assert cloud.deleted_server_ids == ["401"]
            driver.click("retire-done-button")
            recovery_lists_the_box(app, "right after retiring the server")
        finally:
            seat.leave()

        # -- launch 3: a cold start reads only what the device kept ------------
        withhold_optimistic_entry()
        driver = seat.relaunch()
        recovery_lists_the_box(ActionLayer(driver), "after a relaunch")
    finally:
        if seat is not None:
            seat.close()
        stop_boxes()

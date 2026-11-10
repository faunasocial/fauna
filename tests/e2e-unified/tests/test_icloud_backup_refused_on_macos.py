"""On a macOS build that cannot hold the iCloud copy, the toggle says so.

`docs/goal/architecture/apps/ios.md` § Credential Storage, the *Opt-in iCloud
backup* bullet: a synchronizable item needs the data-protection plane, and every
Developer-ID build is on the legacy plane until path A. A toggle left ON there
would tell a person a backup exists when none does. This launch runs the store on
the harness's own legacy keychain file (``real_keychain`` → ``Plane.legacyFile``),
the plane those builds are on, and flips the real toggle through Settings after a
real sign-in.

`test_icloud_backup_copy.py` is the control on a keychain that can hold the copy:
the same code on iOS, two copies landed and the toggle ON. The launch reconcile
clearing a preference that already stands ON is `KeychainCopyNotMoveTests`', since
no UI path can leave one standing any more.
"""

from __future__ import annotations

import contextlib
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from actions import ActionLayer
from helpers.admin_wire import admit_adult
from common.launch_harness import make_launch_harness, sign_in_through_the_wizard
from conftest import _trust_seeder
from helpers.app_launch import app_launch_config
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_identity_key_stays_on_the_device import (
    _keychain_copies,
    _security,
    macos_client,  # noqa: F401 (fixture)
)
from tests.test_onboarding_launch_routing_smoke import smoke_handled_nest  # noqa: F401 (fixture)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.feature("identity-in-your-cloud-keychain"),
]

#: The one copy a legacy-plane row rests as: no sync flag and no class, which the
#: platform does not honour there.
WORKING_COPY = {"accessible": "nil", "synchronizable": False}

#: A flip is one local keychain pass; this is a deadline, never a wait.
_FLIP_BUDGET_S = 30


def test_the_toggle_cannot_rest_on_where_no_copy_can_land(
    macos_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """1. Toggle on → the page's ``error-message`` names the failure and the toggle
    reads OFF.
    2. The keychain agrees: the identity row is still exactly its one working copy,
    and the app still reads the identity from it.
    3. Nothing outlives the test: the reset sweeps the row, and the user's search
    list never saw any of it."""
    nest = smoke_handled_nest
    actor = admit_adult(nest, "cloudrefused", domain=nest["authority"], token=True)
    public = SigningKey(bytes(actor["signing_key"])).verify_key.encode()
    account = f"fauna/{public.hex()}/secret"
    search_list = _security("list-keychains", "-d", "user").stdout

    harness = make_launch_harness(
        "macos", tmp_path=tmp_path, **app_launch_config("macos", request),
        seed_trust=_trust_seeder(request),
        extra_launch_config={"real_keychain": True},
    )
    driver = harness.launch(node_url=nest["url"], trust=nest)
    swept = False
    try:
        keychain = driver.keychain_path
        assert keychain and Path(keychain).is_file(), (
            f"the launch made no keychain file ({keychain!r}) — this launch is not on "
            "the legacy plane's harness keychain, so nothing below would mean anything"
        )
        sign_in_through_the_wizard(driver, actor, nest)
        app = ActionLayer(driver)
        app.settings.open_icloud_backup()
        assert app.settings.icloud_backup_state() == "off", (
            "the toggle must start OFF, or the flip below proves nothing"
        )

        # ── 1. The flip is refused, out loud. ────────────────────────────────
        app.settings.set_icloud_backup(True)
        wait_until(
            lambda: driver.is_visible("error-message"),
            _FLIP_BUDGET_S,
            diagnose=lambda: (
                f"no error-message after the flip; the toggle reads "
                f"{app.settings.icloud_backup_state()!r}: "
                f"{driver.diagnose('settings-icloud-backup-toggle')}"
            ),
        )
        shown = driver.get_text("error-message")
        assert shown == S.settings.errors.icloud_backup, (
            f"the page's error-message reads {shown!r}, not the failed backup"
        )
        assert app.settings.icloud_backup_state() == "off", (
            "the toggle rests ON with no copy in iCloud Keychain: "
            f"{driver.diagnose('settings-icloud-backup-toggle')}"
        )

        # ── 2. The keychain agrees. ──────────────────────────────────────────
        status, copies = _keychain_copies(driver, account)
        assert copies == [WORKING_COPY], (
            f"the identity row rests as {copies} (status {status}) — the flip must "
            "leave exactly the working copy it found"
        )

        # ── 3. Nothing outlives the test. ────────────────────────────────────
        driver.reset()
        swept = True
        status, copies = _keychain_copies(driver, account)
        assert copies == [], f"the reset left {account} in the keychain file: {copies}"
    finally:
        if not swept:
            with contextlib.suppress(Exception):
                driver.reset()
        harness.teardown()
    assert _security("list-keychains", "-d", "user").stdout == search_list, (
        "the launch changed the user's keychain search list"
    )

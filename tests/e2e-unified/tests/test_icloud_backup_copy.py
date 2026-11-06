"""The opt-in iCloud copy on a real keychain: copy-not-move, and what a restored
device finds.

`docs/features/identity-in-your-cloud-keychain.md` outcomes 2 and 3;
`docs/goal/architecture/apps/ios.md` § Credential Storage, the *Opt-in iCloud
backup* bullet and the keychain witness seam. These are keychain-resident facts,
so each launch runs the store on the seat simulator's own keychain
(``real_keychain``), which keeps a ``synchronizable`` item as a device does: the
toggle is the real one, driven through Settings after a real sign-in, and every
read is the attributes-only ``keychain_item_attributes``.

iCloud's absence on the simulator costs nothing but the propagation itself. Two
devices in one iCloud-Keychain circle are the manual last inch, and neither test
asserts of the app what only iCloud does.

The toggle's own round trip through the UI is `test_icloud_backup_toggle.py`'s.
macOS's cells wait for the data-protection plane (path A), in convention 12's
venue; what the toggle does until then is `test_icloud_backup_refused_on_macos.py`'s.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
from types import SimpleNamespace

import pytest
from nacl.signing import SigningKey

from actions import ActionLayer
from helpers.admin_wire import admit_adult
from common.launch_harness import (
    FEED_TAB,
    make_launch_harness,
    reached_authenticated_app,
    sign_in_through_the_wizard,
)
from conftest import _trust_seeder
from helpers.app_launch import app_launch_config
from helpers.relaunch import pin_store_across_relaunch
from helpers.waiting import await_session_actor, wait_until
from tests.test_identity_key_stays_on_the_device import (
    _encodings,
    _keychain_copies,
    ios_client,  # noqa: F401 (fixture)
)
from tests.test_onboarding_launch_routing_smoke import smoke_handled_nest  # noqa: F401 (fixture)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.ios,
    pytest.mark.feature("identity-in-your-cloud-keychain"),
]

DEVICE_BOUND = {
    "accessible": "kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly",
    "synchronizable": False,
}
CIRCLE = {"accessible": "kSecAttrAccessibleAfterFirstUnlock", "synchronizable": True}

#: `KeychainStore.backupPreferenceAccount`, the device-local preference row.
PREFERENCE_ROW = "icloud_backup_enabled"

#: `INSTALL_DEVICE_SECRET`, the install secret every device id derives from.
INSTALL_SECRET_ROW = "install/device_secret"

#: A toggle's rewrite is one local keychain pass; this is a deadline, never a wait.
_COPIES_BUDGET_S = 30


def _copies(driver, account: str) -> list[dict]:
    """The copies of ``account``, device-bound first: the keychain's own order is
    not a fact about the app. A list, so a duplicated copy still shows."""
    _, copies = _keychain_copies(driver, account)
    return sorted(copies, key=lambda copy: copy["synchronizable"])


def _await_copies(driver, account: str, want: list[dict], *, what: str) -> None:
    wait_until(
        lambda: _copies(driver, account) == want,
        _COPIES_BUDGET_S,
        diagnose=lambda: f"{what}: {account} rests as {_copies(driver, account)}, wanted {want}",
    )


def _assert_holds_the_identity(seat) -> None:
    """The row holds this identity's secret, read through the store's own path
    and reported only as a digest."""
    report = json.loads(seat.driver.call_command(
        "keychain_item_attributes", {"account": seat.account, "digest": True}
    ))
    assert report.get("value_sha256") in {
        hashlib.sha256(e).hexdigest() for e in _encodings(seat.secret)
    }, f"the {seat.account} row no longer loads as an encoding of the identity secret"


def _value_digest(driver, account: str) -> str | None:
    """The digest of what the store's own ``load`` returns for ``account``, ``None``
    when it returns nothing. A device row is compared through this, so its value
    never reaches the test."""
    report = json.loads(driver.call_command(
        "keychain_item_attributes", {"account": account, "digest": True}
    ))
    return report.get("value_sha256")


def _await_digest(driver, account: str, *, what: str) -> str:
    return wait_until(
        lambda: _value_digest(driver, account),
        _COPIES_BUDGET_S,
        diagnose=lambda: f"{what}: {account} rests as {_copies(driver, account)}",
    )


def _relaunch_on_the_kept_keychain(seat) -> None:
    """A cold relaunch that finds the keychain as it stands. ``recover()`` re-runs
    this launch's config, whose ``real_keychain`` would sweep the rows, so the
    config moves to ``keep`` first; the driver refuses a ``keep`` with no pin."""
    driver = seat.driver
    pin_store_across_relaunch(driver)
    driver.keep_real_keychain_at_next_launch()
    assert driver.recover(), "the app did not come back up after the relaunch"
    reached_authenticated_app(driver, timeout=90)
    await_session_actor(driver, seat.actor_hex, budget_s=90, what="the relaunch")


def _assert_the_app_finds_the_identity(seat) -> None:
    """The first launch's promise as a person sees it: the feed and their handle,
    with no onboarding between."""
    driver = seat.driver
    driver.wait_for(FEED_TAB, timeout=30)
    handle = wait_until(
        lambda: driver.get_state("session.handle"),
        30,
        diagnose=lambda: f"session={driver.get_state('session')!r}",
    )
    assert seat.handle in handle, (
        f"the app came up as {handle!r}, not as the identity that signed in "
        f"({seat.handle!r})"
    )


def _assert_the_reset_sweeps_the_seat(seat) -> None:
    """No real row outlives the test: the seat's keychain outlives the process."""
    seat.driver.reset()
    seat.swept = True
    for account in (seat.account, PREFERENCE_ROW):
        assert _copies(seat.driver, account) == [], (
            f"the reset left {account} in the seat simulator's keychain"
        )


@contextlib.contextmanager
def _signed_in_seat(request, nest, tmp_path, *, handle: str):
    """A real sign-in through the wizard on a ``real_keychain`` launch."""
    actor = admit_adult(nest, handle, domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    actor_hex = SigningKey(secret).verify_key.encode().hex()
    harness = make_launch_harness(
        "ios", tmp_path=tmp_path, **app_launch_config("ios", request),
        seed_trust=_trust_seeder(request),
        extra_launch_config={"real_keychain": True},
    )
    driver = harness.launch(node_url=nest["url"], trust=nest)
    seat = SimpleNamespace(
        driver=driver, app=ActionLayer(driver), secret=secret, handle=handle,
        actor_hex=actor_hex, account=f"fauna/{actor_hex}/secret", swept=False,
    )
    try:
        assert _copies(driver, seat.account) == [], (
            "the seat simulator's keychain already holds the row before the sign-in "
            "— the launch's sweep did not run"
        )
        sign_in_through_the_wizard(driver, actor, nest)
        _await_copies(driver, seat.account, [DEVICE_BOUND], what="after a real sign-in")
        yield seat
    finally:
        if not seat.swept:
            with contextlib.suppress(Exception):
                driver.reset()
        harness.teardown()


def test_turning_the_copy_off_keeps_this_devices_working_copy(
    ios_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 2. The delete of a synchronizable item propagates circle-wide, and
    that propagation is iCloud's. What the app owns is that no device's only copy
    is ever the circle row: the backup is a COPY beside a device-bound working
    copy, and opting out takes the copy alone. A device that moved its key instead
    would lose it to another device's toggle.

    1. Toggle on → exactly the device-bound copy and the circle copy.
    2. Toggle off → exactly the device-bound copy.
    3. A cold relaunch on that keychain → signed in, on the same row."""
    with _signed_in_seat(
        request, smoke_handled_nest, tmp_path, handle="cloudcopyoff"
    ) as seat:
        seat.app.settings.open_icloud_backup()

        seat.app.settings.set_icloud_backup(True)
        _await_copies(
            seat.driver, seat.account, [DEVICE_BOUND, CIRCLE], what="after the toggle on"
        )

        seat.app.settings.set_icloud_backup(False)
        _await_copies(seat.driver, seat.account, [DEVICE_BOUND], what="after the toggle off")

        _relaunch_on_the_kept_keychain(seat)
        _assert_the_app_finds_the_identity(seat)
        assert _copies(seat.driver, seat.account) == [DEVICE_BOUND]
        _assert_holds_the_identity(seat)

        _assert_the_reset_sweeps_the_seat(seat)


def test_a_restored_device_finds_the_identity_from_the_circle_copy(
    ios_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 3. A restored device IS a keychain holding only the synchronizable
    copies: the platform keeps ``ThisDeviceOnly`` items out of a restore and brings
    synchronizable ones through iCloud Keychain.
    ``keychain_drop_device_bound_copies`` leaves exactly that set, the preference
    row gone with the rest, since it is device-bound too.

    1. Toggle on, drop → the row is exactly the circle copy, and no preference.
    2. A cold relaunch on that keychain → the app finds the identity at first
       launch: the feed, the handle, no onboarding.
    3. The launch reconcile has re-landed the device-bound working copy, carrying
       the secret, so the device no longer depends on the circle row.

    The circle copy's fate after that relaunch is deliberately not asserted: with no preference on the device the reconcile
    reads an opt-out, and whether a restore may delete the circle row is an owner's
    ruling."""
    with _signed_in_seat(
        request, smoke_handled_nest, tmp_path, handle="cloudcopyrestore"
    ) as seat:
        seat.app.settings.open_icloud_backup()
        seat.app.settings.set_icloud_backup(True)
        _await_copies(
            seat.driver, seat.account, [DEVICE_BOUND, CIRCLE], what="after the toggle on"
        )

        raw = seat.driver.call_command("keychain_drop_device_bound_copies", {})
        assert raw, (
            "keychain_drop_device_bound_copies answered nothing — it is not built into "
            "this app, or it refused (the TestAgent failure names why)"
        )
        assert json.loads(raw)["dropped"] >= 2, f"the drop took almost nothing: {raw}"
        assert _copies(seat.driver, seat.account) == [CIRCLE], (
            "the drop must leave exactly the circle copy, the set a restore leaves"
        )
        assert _copies(seat.driver, PREFERENCE_ROW) == [], (
            "the drop left the preference row, which no restore brings"
        )

        _relaunch_on_the_kept_keychain(seat)
        _assert_the_app_finds_the_identity(seat)
        copies = _copies(seat.driver, seat.account)
        assert copies.count(DEVICE_BOUND) == 1, (
            f"the launch re-landed no device-bound working copy: {copies}"
        )
        _assert_holds_the_identity(seat)

        _assert_the_reset_sweeps_the_seat(seat)


def test_the_copy_carries_the_identity_and_a_restored_device_is_a_new_device(
    ios_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 3, what "finds your identity" means: the identity, enrolling as a new
    device. A device-scoped row has exactly one copy whatever the toggle says, so
    the restore set holds none of them and the restored device mints its own.

    1. Toggle on → the secret has two copies; the install secret, the device id and
       the T10 store-principal slot have one, device-bound.
    2. Drop → the identity's circle copy stays and no device row is left at all.
    3. A cold relaunch on that keychain → signed in as the identity, with a device
       id and an install secret of its own.

    The identity circle copy's fate after that relaunch is not asserted, as in the
    restore test above."""
    with _signed_in_seat(
        request, smoke_handled_nest, tmp_path, handle="cloudcopyscope"
    ) as seat:
        driver = seat.driver
        device_id_row = f"fauna/{seat.actor_hex}/device_id"
        store_principal_row = f"fauna-account-store/{seat.actor_hex}"
        device_rows = (INSTALL_SECRET_ROW, device_id_row, store_principal_row)
        for row in device_rows:
            _await_copies(driver, row, [DEVICE_BOUND], what="after a real sign-in")

        seat.app.settings.open_icloud_backup()
        seat.app.settings.set_icloud_backup(True)
        _await_copies(driver, seat.account, [DEVICE_BOUND, CIRCLE], what="after the toggle on")
        for row in device_rows:
            assert _copies(driver, row) == [DEVICE_BOUND], (
                f"{row} is this device's own and gained a circle copy from the toggle"
            )

        device_id_before = _await_digest(driver, device_id_row, what="before the drop")
        install_secret_before = _await_digest(driver, INSTALL_SECRET_ROW, what="before the drop")

        raw = driver.call_command("keychain_drop_device_bound_copies", {})
        assert raw, (
            "keychain_drop_device_bound_copies answered nothing — it is not built into "
            "this app, or it refused (the TestAgent failure names why)"
        )
        assert _copies(driver, seat.account) == [CIRCLE]
        for row in device_rows:
            assert _copies(driver, row) == [], (
                f"the restore set holds {row}, which would bring the new device up as this one"
            )

        _relaunch_on_the_kept_keychain(seat)
        _assert_the_app_finds_the_identity(seat)

        device_id_after = _await_digest(driver, device_id_row, what="after the relaunch")
        assert device_id_after != device_id_before, (
            "the restored device came up with the other device's id"
        )
        assert (
            _await_digest(driver, INSTALL_SECRET_ROW, what="after the relaunch")
            != install_secret_before
        ), "the restored device came up with the other device's install secret"
        for row in device_rows:
            _await_copies(driver, row, [DEVICE_BOUND], what="after the relaunch")

        _assert_the_reset_sweeps_the_seat(seat)

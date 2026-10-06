"""tier_3 — a new folder starts with the devices picked in the wizard, each with
the place flags chosen there (`ui/folders.md` § Layout & flow, the wizard's
device-places step; `wizard-device-check` + `wizard-device-{originates,accepts,
applies-deletes}`, IDs user-approved with slice e's 2026-08-15 grant).

The wizard's second step is the only place a device's part in a folder is
chosen before the folder exists, and until this test nothing ever ticked a box
there: every wizard-driven test in the tree walks straight past the step with
nothing enrolled. The row's editor (`test_folder_place_editor.py`) proves a
seat can be *changed*; this proves the seats a user picks at creation are the
seats the folder is *born* with.

MUTATION is UI-driven (convention 8): the devices are ticked and their flags set
in the wizard, and the folder is created by its Create button. The two device
registrations are fixture setup (the documented carve-out: a user's second and
third machine are arranged, not driven). VERIFICATION reads two witnesses that
cannot agree by accident: the nest's roster (`fauna.folders.members.list`, the
ground truth `test_folder_place_editor.py` also reads) and the row's own
place editor, which repaints from that roster.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import sync_register
from helpers.set_names import addressed
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui is the lead app. The wizard step is built on all seven (the IDs are in
    # every app's ui-actual); the other six join here as their runs land.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]

# Two seats with deliberately DIFFERENT flag sets, neither of them the `sync`
# point every enrollment defaults to — a wizard that dropped the flags and sent
# its default would fail both, and one that swapped the two devices' flags
# would fail on each seat's own assertion.
_ARCHIVE = {"originates": True, "accepts": True, "applies_deletes": False}
_RECEIVE_ONLY = {"originates": False, "accepts": True, "applies_deletes": True}
_FLAG_IDS = {
    "originates": "originates",
    "accepts": "accepts",
    "applies_deletes": "applies-deletes",
}


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _roster(nest_instance, test_user, folder: str) -> list[dict]:
    with _user_client(nest_instance, test_user) as client:
        return client.call("fauna.folders.members.list", addressed(folder))["members"]


def _wizard_device_index(driver, label: str) -> int | None:
    for i in range(driver.count("wizard-device-check")):
        if label in driver.get_text("wizard-device-check", index=i):
            return i
    return None


@pytest.mark.feature("folders")
def test_a_new_folder_starts_with_the_devices_and_places_picked_in_the_wizard(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    b = app.backups
    tag = secrets.token_hex(3)
    seats = {
        f"wiz-archive-{tag}": (bytes([0xC1]) + secrets.token_bytes(31), _ARCHIVE),
        f"wiz-receive-{tag}": (bytes([0xC2]) + secrets.token_bytes(31), _RECEIVE_ONLY),
    }

    # Fixture: two more of this user's devices, registered through the same door
    # every app's first sync uses. They are what the wizard's device step lists.
    # `sync_register` seals each label through the real funnel — a sealless
    # register rests the device nameless (the S9 flip), and a nameless device is
    # one the user could not tell apart in the wizard, nor could this test.
    for label, (device_id, _flags) in seats.items():
        sync_register(
            nest_instance["port"],
            secret_key=test_user["signing_key"].encode().hex(),
            device_id=device_id.hex(),
            label=label,
            base_url=nest_instance["url"],
        )

    # The wizard is seeded, at the moment it opens, from the device list the
    # page last loaded (`DevicesMachine::open_wizard`) — so wait until that list
    # holds both devices before opening it. The roster page reads the same
    # machine, and its `device-name` cards are the observable.
    b.navigate_devices()
    wait_until(
        lambda: all(
            any(label in b.device_name(i) for i in range(b.device_count())) for label in seats
        ),
        30.0,
        diagnose=lambda: (
            f"the device roster never listed both registered devices: "
            f"{[b.device_name(i) for i in range(b.device_count())]!r}"
        ),
    )
    b.navigate_folders()

    name = f"wizplaces-{tag}"
    before = b.folder_count()
    b.add_folder()
    app.driver.wait_for("wizard-name-input", timeout=15.0)
    b.wizard_set_name(name)
    b.wizard_next()

    # ── Step 2 — tick both devices and give each its own place. ──
    app.driver.wait_for("wizard-device-check", timeout=15.0)
    indices = {}
    for label in seats:
        # Wrapped: index 0 is a real answer, and a bare 0 reads as "not yet"
        # to `wait_until`.
        (indices[label],) = wait_until(
            lambda label=label: (
                None if (i := _wizard_device_index(app.driver, label)) is None else (i,)
            ),
            15.0,
            diagnose=lambda label=label: (
                f"the wizard's device step never listed {label!r}: "
                f"{[app.driver.get_text('wizard-device-check', index=i) for i in range(app.driver.count('wizard-device-check'))]!r}"
            ),
        )
    for label, (_device_id, flags) in seats.items():
        i = indices[label]
        b.wizard_check_device(i)
        for flag, on in flags.items():
            b.wizard_set_device_flag(_FLAG_IDS[flag], on, index=i)
        for flag, on in flags.items():
            eid = f"wizard-device-{_FLAG_IDS[flag]}"
            wait_until(
                lambda eid=eid, on=on, i=i: app.driver.get_attr(eid, "state", index=i)
                == ("on" if on else "off"),
                10.0,
                diagnose=lambda eid=eid, i=i: (
                    f"{eid}[{i}] reads {app.driver.get_attr(eid, 'state', index=i)!r}"
                ),
            )
    b.wizard_next()

    # ── Step 3 — create. ──
    app.driver.wait_for("wizard-create-button", timeout=15.0)
    b.wizard_create()
    wait_until(
        lambda: b.folder_count() > before,
        30.0,
        diagnose=lambda: f"the folder never appeared; error={app.error_text()!r}",
    )
    assert not app.has_error(), f"creating the folder surfaced an error: {app.error_text()!r}"

    # ── The nest's roster: exactly the picked devices, each with its flags. ──
    roster = _roster(nest_instance, test_user, name)
    by_device = {m["device_id"]: m for m in roster}
    for label, (device_id, flags) in seats.items():
        seat = by_device.get(device_id.hex())
        assert seat is not None, (
            f"{label!r} was ticked in the wizard but the new folder has no seat for "
            f"it: {roster!r}"
        )
        assert seat["flags"] == flags, (
            f"{label!r} was given {flags} in the wizard but the folder was born "
            f"with {seat['flags']}"
        )
    extra = [m for m in roster if m["device_id"] not in {d.hex() for d, _ in seats.values()}]
    assert not extra, (
        f"only the ticked devices take part in the new folder; also enrolled: {extra!r}"
    )

    # ── The row says the same: its place editor paints each seat, BY NAME. ──
    # The roster's plaintext label rests empty since labels were sealed, so the
    # name on a `folder-place-row` is the app's own unsealed device roster joined
    # on `device_id` (`place_rows`). Matching by name is what proves that join:
    # a row painting blank, or painting one device's name over another's flags,
    # fails here.
    b.find_and_expand_folder(name)
    wait_until(
        lambda: app.driver.count("folder-place-row") == len(roster),
        15.0,
        diagnose=lambda: (
            f"the expanded row should paint one place row per seat ({len(roster)}); "
            f"it paints {app.driver.count('folder-place-row')}"
        ),
    )
    row_names = app.driver.get_texts("folder-place-row")
    for label, (_device_id, flags) in seats.items():
        named = [j for j, text in enumerate(row_names) if label in text]
        assert len(named) == 1, (
            f"exactly one place row should be named {label!r}; the rows read {row_names!r}"
        )
        scope = f"folder-place-row[{named[0]}]"
        for flag, on in flags.items():
            eid = f"folder-place-{_FLAG_IDS[flag]}"
            assert app.driver.get_attr(eid, "state", scope=scope) == ("on" if on else "off"), (
                f"{label!r}'s {eid} should read {'on' if on else 'off'} on its row, "
                f"as the wizard set it"
            )

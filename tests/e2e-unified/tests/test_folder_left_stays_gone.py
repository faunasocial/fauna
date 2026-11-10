"""tier_3 — folders outcome 19: *a folder you leave on one device does not come
back on your other devices* (`docs/features/folders.md` § Coverage contract;
`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync → *A
sibling's deletion is not a join*).

A folder leave is the self-scoped roster drop plus `forget_group` on the
leaving device. It writes no MLS commit, so the account's other device still
holds the group, and a device that carried it would list it again at its next
flush and resurrect the folder for the leaver. The ruling makes the other
device's launch and resync drop a group its last-seen listing names and the
replica no longer lists. The mechanism is pinned red-first below the UI
(`fauna-client-mls-sync::sync`'s `a_folder_left_on_one_device_stays_left_across_a_siblings_*`);
this is the user-visible outcome across two real seats of one account.

Flow (convention 16, two seats on this machine; every mutation a UI gesture,
convention 8, except the stranger's share, which is fixture arrangement as in
`test_folders.py`'s shared-with-me journey):

1. A fresh account signs seat A in. A stranger shares a folder with it, minted
   against seat A's KeyPackage, before the second seat exists, so the Welcome
   is seat A's to consume. Seat A accepts.
2. The account's second device, seat B, launches and lists the folder (it
   reaches B through the account's replica, never a second Welcome).
3. Seat B leaves the folder. Both seats stop listing it.
4. Seat A restarts as the same device (the launch door), and then makes a new
   folder, which seat B must list: that proves A's post-restart state reached
   B. The left folder must be absent on both seats throughout.
"""

import secrets
import time

import pytest

from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

#: A seat's own write: the page's round trip (`test_folders.py`'s budget).
SEAT_WRITE_S = 120.0

#: One seat's change reaching the other: the publish, the nest's relay, the
#: other seat's walk and its page reload (`test_folders.py`'s budget).
CONVERGE_S = 240.0


@pytest.fixture
def sibling_seats(request, nest_instance):
    """Second enrolled seats on the session nest (`helpers.fleet.SiblingSeats`)."""
    from helpers.fleet import SiblingSeats

    seats = SiblingSeats(request, nest_instance)
    try:
        yield seats
    finally:
        seats.teardown()


def _share_from_a_stranger(nest: dict, recipient: dict) -> str:
    """A fresh stranger owns a group-bound folder and delivers its Welcome to
    ``recipient`` (the knock `test_folders.py`'s shared-with-me journey seeds).
    Returns the folder's name."""
    from conftest import MAIL_PRIMARY_DOMAIN
    from helpers.admin_wire import admit_adult
    from tests.api import conv_api

    port = nest["port"]
    stranger = admit_adult(
        nest, "lvr" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN, token=True
    )
    deadline = time.monotonic() + 30
    kp = None
    while time.monotonic() < deadline:
        kp = conv_api.keypackage_fetch(port, stranger, recipient["actor_id_hex"])
        if kp:
            break
        time.sleep(1)  # sleep-ok: bounded poll cadence inside the deadline
    assert kp, "the recipient never published a fetchable KeyPackage for the sharer"
    channel_id_hex, welcome_bytes, group_id_hex = conv_api.mint_group_welcome(
        bytes(stranger["signing_key"]), kp
    )
    name = "left-" + secrets.token_hex(3)
    conv_api.folder_create(port, stranger, name)
    conv_api.folder_share(port, stranger, name, group_id_hex)
    # `kind.group_id` is the RAW MLS group id the leave later derives the
    # channel from (`test_folders.py` says why a channel id here no-ops it).
    conv_api.welcome_deliver(
        port,
        stranger,
        recipient["actor_id_hex"],
        channel_id_hex,
        welcome_bytes,
        kind={"type": "folder", "group_id": group_id_hex},
    )
    return name


def _lists(seat, name: str) -> bool:
    try:
        return any(name in t for t in seat.backups.folder_titles())
    except LookupError:
        return False  # a row the list rebuilt away between the count and the read


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.real_conversations
# Documented-long (convention 9): two seats, a share, a leave, a restart and a
# convergence round after it.
@pytest.mark.timeout(1200)
@pytest.mark.feature("folders")
def test_a_folder_left_on_one_device_does_not_come_back_on_the_other(
    app, request, nest_instance, sibling_seats
):
    from conftest import _make_user
    from helpers import enrollment
    from helpers.fleet import poked_pass
    from helpers.waiting import await_account_runtime_assembled

    url = nest_instance["url"]
    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, url, user)
    # The restart in step 4 must be THIS device restarting, not a new one.
    assert app.driver.preserve_state_across_relaunch(), (
        "the seat could not pin its client store across the relaunch, so the "
        "restart half of this outcome would be vacuous"
    )

    # ── 1. Seat A accepts a stranger's share. ──
    name = _share_from_a_stranger(nest_instance, user)
    a = app.backups
    a.navigate_folders()
    assert a.wait_for_pending_shares(1) == 1, (
        f"the stranger's share never staged on seat A; error={app.error_text()!r}"
    )
    a.accept_pending_share(0)
    assert a.wait_for_pending_shares(0) == 0, (
        f"accepting never consumed the knock; error={app.error_text()!r}"
    )
    wait_until(
        lambda: name in " ".join(a.revisit_folders(settle=0)),
        SEAT_WRITE_S,
        interval=1.0,
        diagnose=lambda: f"seat A never listed the accepted {name!r}; error={app.error_text()!r}",
    )

    # ── 2. The account's second device lists it too. ──
    seat_b = sibling_seats.launch(user=user)
    enrollment.await_enrollment(seat_b, url, user)
    b = seat_b.backups
    b.navigate_folders()

    def b_lists_it() -> bool:
        poked_pass(app.driver, what="seat A's publish")
        poked_pass(seat_b.driver, what="seat B's walk")
        return name in " ".join(b.revisit_folders(settle=0))

    wait_until(
        b_lists_it,
        CONVERGE_S,
        interval=1.0,
        diagnose=lambda: (
            f"seat B, the same account's other device, never listed {name!r}: "
            f"{b.revisit_folders()}; error={seat_b.error_text()!r}"
        ),
    )

    # ── 3. Seat B leaves it; both seats stop listing it. ──
    b.navigate_folders()
    b.wait_for_folder_row(name)
    # The leave is a suffix on the read-only member row; the fresh account has
    # no other member row, so index 0 is this folder's.
    seat_b.driver.wait_for("folder-leave-button", timeout=15.0)
    b.leave_shared_set(0)
    wait_until(
        lambda: name not in " ".join(b.revisit_folders(settle=0)),
        SEAT_WRITE_S,
        interval=1.0,
        diagnose=lambda: f"seat B still lists the folder it left; error={seat_b.error_text()!r}",
    )

    def a_dropped_it() -> bool:
        poked_pass(seat_b.driver, what="seat B's publish")
        poked_pass(app.driver, what="seat A's walk")
        return name not in " ".join(a.revisit_folders(settle=0))

    wait_until(
        a_dropped_it,
        CONVERGE_S,
        interval=1.0,
        diagnose=lambda: f"seat A still lists {name!r} after seat B left it: {a.revisit_folders()}",
    )

    # ── 4. Seat A restarts as the same device, then publishes something new. ──
    app.driver.hard_reload()
    await_account_runtime_assembled(app.driver)
    assert not _lists(app, name) and name not in " ".join(a.revisit_folders()), (
        f"after seat A's restart the folder seat B left is back on seat A: "
        f"{a.folder_titles()}"
    )

    marker = f"after-restart-{secrets.token_hex(4)}"
    a.navigate_folders()
    a.create_folder_via_wizard(marker)

    def b_saw_a_after_restart() -> bool:
        poked_pass(app.driver, what="seat A's post-restart publish")
        poked_pass(seat_b.driver, what="seat B's walk")
        return marker in " ".join(b.revisit_folders(settle=0))

    wait_until(
        b_saw_a_after_restart,
        CONVERGE_S,
        interval=1.0,
        diagnose=lambda: (
            f"seat B never listed {marker!r}, made on seat A after its restart: "
            f"{b.revisit_folders()}; error={seat_b.error_text()!r}"
        ),
    )
    # Seat A's post-restart state has reached seat B: the left folder stays gone
    # on both.
    assert name not in " ".join(b.revisit_folders()), (
        f"the folder seat B left came back on seat B after seat A's restart: "
        f"{b.folder_titles()}"
    )
    assert name not in " ".join(a.revisit_folders()), (
        f"the folder seat B left came back on seat A: {a.folder_titles()}"
    )
    assert not app.has_error(), f"seat A surfaced an error: {app.error_text()!r}"
    assert not seat_b.has_error(), f"seat B surfaced an error: {seat_b.error_text()!r}"

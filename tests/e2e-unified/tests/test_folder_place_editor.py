"""tier_3 — the post-create device-place editor (`folder-place-row` + the three
flag checkboxes; folders re-model phase 2, IDs user-approved with slice e's
2026-08-15 grant, built 2026-08-19 with tui leading).

Slice b landed `fauna.folders.places.set` and slice f made every flag point
writable — with **no app able to reach an existing seat's flags** (the wizard
sets them only at creation). This is the test that the editor closes that gap:
a seat's place is edited *in place*, from the expanded folder row, and the row
repaints the NEST's answer (the gesture re-reads the device roster rather than
flipping optimistically).

MUTATION is UI-driven (convention 8 — the checkbox is clicked, never a raw
`places.set`); the folder + its seat are fixture setup (the documented
carve-out), and VERIFICATION reads the nest's ground truth through
`fauna.folders.members.list`, the sibling `test_folder_nest_place.py`'s
black-box idiom.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import PLACE_SYNC, place_flags_payload
from helpers import enrollment
from helpers.set_names import addressed
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui is the lead app; the other six join as their
    # legs land — the marker list is the parity ledger, exactly as its sibling
    # `test_folder_nest_place.py` keeps it.
    pytest.mark.tui,
    # web, 2026-08-28: the second app. Its seats are projected by the same
    # shared `place_rows` tui paints from, so this test asserting the SAME
    # states on both is what proves the projection is one rule and not two.
    pytest.mark.web,
    # linux, 2026-08-28: the third app, off the same `place_rows` projection.
    # Its leg is what made the folder row survive a refresh at all — linux
    # rebuilds the whole list on every snapshot change, and a rebuilt
    # `AdwExpanderRow` starts collapsed, so before it the write below took the
    # boxes off screen instead of repainting them.
    pytest.mark.linux,
    # macos + ios, 2026-08-28: the fourth and fifth, one shared FaunaKit
    # `FolderPlacesSection` for both. They consume `FfiFolderMember`, whose
    # flag triple is filled by the same `place_rows` at the
    # FFI boundary — so a fourth and fifth app asserting these exact states is
    # more evidence that the projection is one rule and not five.
    pytest.mark.macos,
    pytest.mark.ios,
    # windows, 2026-08-29: the sixth, and the first leg on a toolkit whose
    # `get_attr(id, "state")` read resolves to AutomationProperties.HelpText —
    # a WinUI CheckBox's own ToggleState is an enum, not the "on"/"off" this
    # contract wants, so each box stamps the two literals itself. Six apps now
    # assert these exact states off the one `place_rows` projection.
    pytest.mark.windows,
]

def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _seat(client, folder: str, device_id: str) -> dict:
    with client:
        reply = client.call("fauna.folders.members.list", addressed(folder))
    seats = [m for m in reply["members"] if m["device_id"] == device_id]
    assert seats, f"the seeded seat must be on the roster: {reply['members']!r}"
    return seats[0]


@pytest.mark.feature("folders")
def test_device_place_editor_edits_a_seat_in_place(logged_in_app, nest_instance, test_user):
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    # Fixture: a folder (via the ordinary wizard) plus one enrolled device
    # seat, seeded through the same doors the wizard itself uses.
    name = f"places-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    # The seat's device registers SEALED, through the shared helper: the nest
    # rests no plaintext label, so a sealless register leaves a nameless row on
    # the session-shared user that every later module's Devices page inherits
    # (on windows the nameless card read as a missing one — measured 2026-09-29,
    # this module ahead of test_device_cards.py).
    device_id = enrollment.register_device(nest_instance["url"], test_user, "editor-seat")
    client = _user_client(nest_instance, test_user)
    with client:
        client.call(
            "fauna.folders.places.set",
            addressed(name, device_id=device_id, flags=place_flags_payload(PLACE_SYNC)),
        )

    # The editor renders on expand: one seat, three boxes, all three on.
    row = b.find_and_expand_folder(name)
    app.driver.wait_for("folder-place-row", timeout=15.0)
    scope = "folder-place-row[0]"
    assert app.driver.get_attr("folder-place-applies-deletes", "state", scope=scope) == "on"
    assert app.driver.get_attr("folder-place-accepts", "state", scope=scope) == "on"
    assert app.driver.get_attr("folder-place-originates", "state", scope=scope) == "on"

    # ── Edit 1: → the archive point (applies_deletes off). ──
    app.driver.click("folder-place-applies-deletes", scope=scope)
    wait_until(
        lambda: app.driver.get_attr("folder-place-applies-deletes", "state", scope=scope)
        == "off",
        15.0,
        diagnose=lambda: (
            f"state={app.driver.get_attr('folder-place-applies-deletes', 'state', scope=scope)!r} "
            f"error={app.error_text()!r}"
        ),
    )
    seat = _seat(client, name, device_id)
    assert seat["flags"] == {"originates": True, "accepts": True, "applies_deletes": False}
    assert "role" not in seat, f"the role contraction retired the member `role`: {seat!r}"

    # ── Edit 2: → originates-only (accepts off too) — delivery switched off for
    # this seat, the flag slice c made real. ──
    app.driver.click("folder-place-accepts", scope=scope)
    wait_until(
        lambda: app.driver.get_attr("folder-place-accepts", "state", scope=scope) == "off",
        15.0,
        diagnose=lambda: f"error={app.error_text()!r}",
    )
    seat = _seat(client, name, device_id)
    assert seat["flags"] == {"originates": True, "accepts": False, "applies_deletes": False}

    # The repaint survives a collapse + re-expand — the row shows nest truth,
    # not a lingering local flip.
    b.expand_folder(row)  # collapse (the expander is a toggle)
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-place-row", timeout=15.0)
    assert app.driver.get_attr("folder-place-accepts", "state", scope=scope) == "off"
    assert app.driver.get_attr("folder-place-applies-deletes", "state", scope=scope) == "off"

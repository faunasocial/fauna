"""tier_3 — the exclusive-editing control and its status line
(`folder-exclusive-editing-toggle`, `folder-lease-status`; tui leads).

Owner docs: `docs/goal/ui/folders.md` § Exclusive editing (the surface) and
`docs/goal/behavior/file-sync.md` § Exclusive editing (the lease).

What the rest of the tree already pins, and what this adds. The nest half is
`tests/api/test_folder_exclusive_lease.py`; the engine's hold → refusal →
release → takeover is `bins/fauna-nest/tests/conformance_folder_lease_two_seats.rs`.
Neither has an app in it. This file drives the app:

1. **The toggle writes the nest's property and paints the nest's answer.** Off
   by default, flipped through the UI (convention 8), verified on the nest row.
2. **The status line reads the projection.** Another device of the same
   account takes the lease — that device's act, not this user's gesture in this
   app, so it goes over the wire as a second seat would — and the line names it
   by its LABEL, never the 64-character id, and says the local edits are kept.
   Released, it reads free again. The app never probes by acquiring: if it
   did, the other device's acquire below would be refused and the test would
   fail at the setup step, not silently pass.
3. **Off again, the line is gone.**

tui has no timer on the folders page — it re-reads the folder list on the
navigation edge and after every folder gesture — so the test re-visits the page
to observe another device's change. The wait is a deadline poll on state
(convention 14), never a sleep.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import enrollment
from helpers.set_names import addressed, find_set
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui leads; the other six join through the trickle-down row.
    pytest.mark.tui,
]

_FLAG_WINDOW_SECS = 30.0
_UI_WINDOW_SECS = 15.0

# A second device of the same account, registered the way a real sync
# daemon registers — its label SEALED under the owner's root
# (`enrollment.register_device`). A sealless register rests no label at all
# since the S9 flip, and the line would then (correctly) say "another device".
_OTHER_LABEL = "Studio desktop"


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _folder_row(client, name: str) -> dict | None:
    with client:
        reply = client.call("fauna.folders.list", {})
    return find_set(reply.get("folders", []), name)


def _await_flag(client, name: str, on: bool) -> None:
    wait_until(
        lambda: (row := _folder_row(client, name)) is not None
        and bool(row.get("exclusive_editing")) is on,
        _FLAG_WINDOW_SECS,
        interval=0.5,
        diagnose=lambda: f"wanted exclusive_editing={on}, nest row is {_folder_row(client, name)!r}",
    )


@pytest.mark.feature("folders")
def test_exclusive_editing_toggle_and_lease_status(logged_in_app, nest_instance, test_user):
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"exclusive-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    client = _user_client(nest_instance, test_user)

    def revisit_and_read_status():
        # Leave and come back: the navigation edge is tui's re-read of the list.
        app.driver.set_state({"nav": {"stack": [{"view": "settings"}]}})
        b.navigate_folders()
        return b.lease_status_text(b.wait_for_folder_row(name))

    # ── 1. Off by default: toggle paints off, no status line. ──
    row = b.find_and_expand_folder(name)
    app.driver.wait_for("folder-exclusive-editing-toggle", timeout=_UI_WINDOW_SECS)
    assert b.exclusive_editing_state() == "off"
    assert b.lease_status_text(row) is None, "the line exists only while the folder is governed"

    # ── 2. Flip it through the UI; the nest moves and the line reads free. ──
    b.toggle_exclusive_editing()
    _await_flag(client, name, True)
    wait_until(
        lambda: b.lease_status_text(b.wait_for_folder_row(name)) == S.devices.folder_lease_free,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"status={b.lease_status_text(b.wait_for_folder_row(name))!r} error={app.error_text()!r}",
    )

    # ── 3. Another device takes the lease; the line names it by label. ──
    other_device = enrollment.register_device(nest_instance["url"], test_user, _OTHER_LABEL)
    with client:
        # Would be refused if the app had taken the lease to "ask" about it.
        assert client.call(
            "fauna.folders.lease.acquire", addressed(name, device_id=other_device)
        )["acquired"] is True
    held = S.devices.folder_lease_held_by(device=_OTHER_LABEL)
    wait_until(
        lambda: revisit_and_read_status() == held,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"wanted {held!r}, status={revisit_and_read_status()!r}",
    )
    assert other_device not in held

    # ── 4. Released, it reads free again. ──
    with client:
        assert client.call(
            "fauna.folders.lease.release", addressed(name, device_id=other_device)
        )["released"] is True
    wait_until(
        lambda: revisit_and_read_status() == S.devices.folder_lease_free,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"status after release={revisit_and_read_status()!r}",
    )

    # ── 5. Off again through the UI; the line is gone. ──
    b.find_and_expand_folder_until(name, "folder-exclusive-editing-toggle", timeout=_UI_WINDOW_SECS)
    b.toggle_exclusive_editing()
    _await_flag(client, name, False)
    wait_until(
        lambda: b.lease_status_text(b.wait_for_folder_row(name)) is None,
        _UI_WINDOW_SECS,
        diagnose=lambda: f"status still {b.lease_status_text(b.wait_for_folder_row(name))!r}",
    )

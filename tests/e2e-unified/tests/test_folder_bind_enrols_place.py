"""tier_3 — **a local presence writes the place it needs** (`file-sync.md` § 4).

A folder created with no device checked holds no place for this device. Binding
a location under it is the gesture that gives the device a local presence, so
the bind ENROLS the device at the default point (`fauna.folders.places.set`,
all three flags) — one shared implementation, the agent's `SetLocationFolder`
handling calling `FoldersClient::ensure_place` — and the expanded row's place
editor (`folder-place-row`, the sibling `test_folder_place_editor.py`'s
surface) repaints to show the new seat without a collapse/re-expand.

MUTATION is UI-driven (convention 8 — the location is bound through the form);
VERIFICATION reads the nest's ground truth through `fauna.folders.members.list`,
then the repainted row.

"A re-bind writes nothing" is pinned below the UI, where it is deterministic:
`bins/fauna-nest/tests/conformance_folders.rs` (the shared helper) and
`bins/fauna-sync-agent/tests/agent_process_tier3.rs` (the real agent process).
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.folder_content import bind_location_under_set
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui leads. linux and windows bind through the SAME agent seam, so their
    # legs are a run, not a build; apple and android join when their
    # on-demand toggle calls the enrol. Grow this list and the parametrization
    # below together.
    pytest.mark.tui,
]

# Generous ceiling (convention 14): the bind's agent round trip, the agent's
# enrol (`members.list` + `places.set`) and the row's roster re-read.
_SEAT_WINDOW_SECS = 60.0


@pytest.mark.parametrize("folder_share_owner_app", ["tui"], indirect=True)
@pytest.mark.real_conversations
# The bind must reach a real agent — the seam under test lives there.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
# Documented-long (convention 9): one GUI app + one real sync agent + a bind.
@pytest.mark.timeout(900)
@pytest.mark.feature("folders")
def test_binding_a_location_gives_a_placeless_device_its_seat(
    folder_share_owner_app, tmp_path
):
    app, nest, owner = folder_share_owner_app
    client = WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    name = f"seat-{secrets.token_hex(4)}"

    def roster() -> list:
        with client:
            return client.call("fauna.folders.members.list", {"name": name})["members"]

    app.backups.navigate_folders()
    app.backups.create_folder_via_wizard(name)  # no device checked
    assert roster() == [], "precondition: the folder holds no place"

    location = tmp_path / "bound"
    location.mkdir()
    bind_location_under_set(app, name, location, seat="owner")

    wait_until(
        lambda: len(roster()) == 1,
        _SEAT_WINDOW_SECS,
        diagnose=lambda: f"roster={roster()!r} error={app.error_text()!r}",
    )
    seat = roster()[0]
    assert seat["flags"] == {"originates": True, "accepts": True, "applies_deletes": True}

    # The row repaints nest truth with no collapse/re-expand: the confirmed
    # bind re-reads the expanded row's roster.
    app.driver.wait_for("folder-place-row", timeout=_SEAT_WINDOW_SECS)
    scope = "folder-place-row[0]"
    for box in ("folder-place-originates", "folder-place-accepts", "folder-place-applies-deletes"):
        assert app.driver.get_attr(box, "state", scope=scope) == "on", box

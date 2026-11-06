"""tier_3 — showing a folder on demand on a device the folder did not include
enrols that device ONCE, at the default place (`files-on-demand` outcome 9),
through the per-binding switch of a placeholder host — the linux app and tui.

**Same outcome, the binding's mechanism.** Apple's witness is
`test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_enrols_a_place_less_device_once`,
where the set-level `folder-on-demand-toggle` is the enrol gesture
(`on-demand-files-apple.md` § Apple File Provider binding — a sanctioned
platform deviation, `ui/folders.md` § Element IDs). linux has no set-level
switch: a folder is shown on this device by binding a location and is shown ON
DEMAND by that binding's `folder-location-mode-toggle` (`ui/folders.md`
§ Binding; `on-demand-files.md` § Linux FUSE binding). The promise is the same —
a device the folder did not include gains exactly one default place, and asking
for the folder on demand again writes no second one — and it holds here because
the bind runs the shared `ensure_place` (`file-sync.md` § 4, *A local presence
writes the place it needs*), which writes only into a gap.

**The flow it asserts.** A wizard-made folder starts place-less
(`fauna.folders.create` writes no place) → the user binds a local folder under
it (the page's add flow) → the real `fauna-sync-agent` the launch spawned binds
and enrols this device (`sync_agent.rs::push_bind`) → the user flips the binding
to on-demand and the agent mounts its FUSE root over the folder (read from the
kernel's mount table) → the nest roster (`fauna.folders.members.list`) holds
exactly one new row, with the default flags → flipping the switch off and on
again leaves that row alone and adds none. The MUTATIONS are UI gestures; the
roster read is VERIFICATION only (convention 8).

windows' leg of the switch runs no live agent (the page rides the in-memory
location channel, `test_folder_location_mode_toggle.py`'s docstring), so it has
no enrolment to observe; it is not collected here. tui drives the same real
agent and publishes the session `device_id` this witness reads "once" against.
"""

import pytest

from helpers.waiting import wait_until
from tests.test_folder_location_mode_toggle import (
    _assert_served,
    _bind_fresh_folder,
    _require_placeholder_host,
)

pytestmark = [
    pytest.mark.tier_3,
    # The placeholder hosts whose switch drives a REAL agent and publishes the
    # device id this witness keys on (module docstring for windows).
    pytest.mark.linux,
    pytest.mark.tui,
]


@pytest.mark.feature("files-on-demand")
def test_showing_a_folder_on_demand_enrols_a_place_less_device_once(
    logged_in_app, nest_instance, test_user, request, tmp_path
):
    """Bind + on-demand on a place-less device: one default place, never two."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.set_names import addressed

    app = logged_in_app
    _require_placeholder_host(app)
    loc = app.sync_locations

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )

    def roster(name: str) -> list[dict]:
        # A sealed set rests no plaintext name: address it by its hash.
        with client:
            return client.call("fauna.folders.members.list", addressed(name))["members"]

    # This device's own id, off its published session — the roster is keyed by
    # it, so "once" is read as "exactly one row for THIS device" whatever else
    # the folder holds.
    device_id = (app.driver.get_state() or {}).get("session", {}).get("device_id")
    assert device_id, f"the app published no session device_id: {app.driver.get_state()!r}"

    name, scope, src = _bind_fresh_folder(app, request, tmp_path)

    def enrolled() -> list[dict]:
        return [m for m in roster(name) if m["device_id"] == device_id]

    # Show it ON DEMAND (UI): the binding's switch to on-demand.
    if loc.mode_toggle_state(scope=scope) != "on-demand":
        assert loc.toggle_mode(scope=scope) == "on-demand", (
            f"the switch never moved the binding to on-demand; error={app.error_text()!r}"
        )
    _assert_served(app, src, "on-demand")

    wait_until(
        lambda: len(enrolled()) == 1,
        60.0,
        interval=1.0,
        diagnose=lambda: f"showing {name!r} on demand never enrolled this device exactly "
        f"once; roster={roster(name)!r} error={app.error_text()!r}",
    )
    (seat,) = enrolled()
    flags = seat.get("flags", {})
    assert (flags.get("originates"), flags.get("accepts"), flags.get("applies_deletes")) == (
        True,
        True,
        True,
    ), f"the enrol writes the default place (all three flags); got {seat!r}"

    # Off and on again finds the place and writes nothing.
    assert loc.toggle_mode(scope=scope) == "always"
    _assert_served(app, src, "always")
    assert loc.toggle_mode(scope=scope) == "on-demand"
    _assert_served(app, src, "on-demand")
    assert enrolled() == [seat], (
        "asking for the folder on demand again must write no second row and leave "
        f"the enrolled place untouched; roster={roster(name)!r}"
    )
    assert not app.has_error(), f"the enrol raised an error: {app.error_text()!r}"

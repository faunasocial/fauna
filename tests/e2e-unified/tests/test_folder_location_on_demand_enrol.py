"""tier_3 — showing a folder on demand on a device the folder did not include
enrols that device ONCE, at the default place (`files-on-demand` outcome 9),
through the per-binding switch of a placeholder host — the linux and windows
apps, and tui.

**Same outcome, the binding's mechanism.** Apple's witness is
`test_folder_on_demand_toggle.py::test_folder_on_demand_toggle_on_enrols_a_place_less_device_once`,
where the set-level `folder-on-demand-toggle` is the enrol gesture
(`on-demand-files-apple.md` § Apple File Provider binding — a sanctioned
platform deviation, `ui/folders.md` § Element IDs). linux and windows have no
set-level switch: a folder is shown on this device by binding a location and is
shown ON DEMAND by that binding's `folder-location-mode-toggle` (`ui/folders.md`
§ Binding; `on-demand-files.md` § On-Demand Files — a FUSE root on linux, a
cfapi sync root on windows, `apps/windows.md` § Shell Extension). The promise is
the same — a device the folder did not include gains exactly one default place,
and asking for the folder on demand again writes no second one — and it holds
here because the bind runs the shared `ensure_place` (`file-sync.md` § 4, *A
local presence writes the place it needs*), which writes only into a gap.

**The flow it asserts.** A wizard-made folder starts place-less
(`fauna.folders.create` writes no place) → the user binds a local folder under
it (the page's add flow) → the real `fauna-sync-agent` binds and enrols this
device (`pipe_server.rs::enrol_bound_place`) → the binding is shown on demand: a
fresh linux binding starts always-resident, so there the user flips the switch;
a fresh windows binding already starts on-demand (`on-demand-files.md` § On-Demand
Files, the user ruling of 2026-09-26), so there the bind is itself the gesture →
the agent's on-demand root stands over the folder, read from another process's
view (`on_demand_root_stands`: the kernel's mount table on linux, the filter's
sync-root registration on windows) → the nest roster (`fauna.folders.members.list`)
holds exactly one row for this device, with the default flags → flipping the
switch off and on again brings the root down and back up, and leaves that row
alone and adds none. The MUTATIONS are UI gestures; the roster read is
VERIFICATION only (convention 8).

**Every leg binds through a real agent, or there is no enrolment to observe.**
linux and tui direct-spawn the agent into the launch's isolated world. The
windows app binds into its isolated agent only under the two real-agent markers
(conftest `_apply_isolated_sync_agent_env`) and only while the Folders page is
NOT swapped onto the in-memory location channel — so this module binds with
`bind_location_under_set` (the shape `test_folder_location_free_space.py`
takes), never `test_folder_location_mode_toggle._bind_fresh_folder`, whose
`prepare_binding_channel` makes that swap on windows. Each app publishes the
session `device_id` its agent enrols under (windows: `session.device_id` in
`App.xaml.cs`'s state, the per-account slot `SessionDeviceId` hands the agent),
and "once" is read against it.
"""

import secrets

import pytest

from helpers.folder_content import (
    agent_diagnosis,
    bind_location_under_set,
    mountable_location,
    on_demand_root_stands,
    open_bound_row,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # The placeholder hosts whose switch drives a REAL agent and publishes the
    # device id this witness keys on (module docstring).
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.tui,
    # A placeholder host runs the leg: windows (cfapi) and linux (the agent's
    # FUSE root); a macOS host's bound location is always-resident by design
    # (feature-catalog.md ruling (i): a declaration, not an in-body absence —
    # the same pair the mode-toggle module declares).
    pytest.mark.host_platform("linux", "windows"),
]

# A ceiling, not an expectation: the agent serves or drops the root after it
# has answered the bind or the flip, restarting the folder's engine in between.
_ROOT_WINDOW_SECS = 90.0


def _assert_root(app, src, mode: str) -> None:
    """The binding is not just remembered as ``mode``, it is SERVED: the
    agent's on-demand root stands over ``src`` exactly while it is on-demand —
    read off another process's view on either host, polled against a ceiling."""
    want = mode == "on-demand"
    wait_until(
        lambda: on_demand_root_stands(src) == want,
        _ROOT_WINDOW_SECS,
        interval=0.3,
        diagnose=lambda: (
            f"the binding reads {mode!r} but the on-demand root is "
            f"{'not ' if want else 'still '}standing over {src}; "
            f"error={app.error_text()!r}\n" + agent_diagnosis(app, "owner")
        ),
    )


@pytest.mark.feature("files-on-demand")
# The windows app binds into the real agent only under BOTH real-agent markers
# (conftest `_apply_isolated_sync_agent_env`); tui and linux ignore them.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_showing_a_folder_on_demand_enrols_a_place_less_device_once(
    logged_in_app, nest_instance, test_user, request, tmp_path
):
    """Bind + on-demand on a place-less device: one default place, never two."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.set_names import addressed

    app = logged_in_app
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

    # A fresh place-less folder (UI: the wizard, no device checked) with one
    # local folder bound under it through the page's add flow, into the agent.
    name = f"enrol-{secrets.token_hex(4)}"
    app.backups.navigate_folders()
    app.backups.create_folder_via_wizard(name)
    assert roster(name) == [], "precondition: the folder holds no place"
    src = mountable_location(request, tmp_path, "enrol-src")
    bind_location_under_set(app, name, src, seat="owner")
    scope = open_bound_row(app, name)

    def enrolled() -> list[dict]:
        return [m for m in roster(name) if m["device_id"] == device_id]

    # Show it ON DEMAND (UI): the binding's switch to on-demand — already there
    # on a windows host, where a fresh binding starts on-demand.
    if loc.mode_toggle_state(scope=scope) != "on-demand":
        assert loc.toggle_mode(scope=scope) == "on-demand", (
            f"the switch never moved the binding to on-demand; error={app.error_text()!r}"
        )
    _assert_root(app, src, "on-demand")

    wait_until(
        lambda: len(enrolled()) == 1,
        60.0,
        interval=1.0,
        diagnose=lambda: f"showing {name!r} on demand never enrolled this device "
        f"({device_id}) exactly once; roster={roster(name)!r} error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner"),
    )
    (seat,) = enrolled()
    flags = seat.get("flags", {})
    assert (flags.get("originates"), flags.get("accepts"), flags.get("applies_deletes")) == (
        True,
        True,
        True,
    ), f"the enrol writes the default place (all three flags); got {seat!r}"

    # Off and on again finds the place and writes nothing.
    scope = open_bound_row(app, name)
    assert loc.toggle_mode(scope=scope) == "always"
    _assert_root(app, src, "always")
    scope = open_bound_row(app, name)
    assert loc.toggle_mode(scope=scope) == "on-demand"
    _assert_root(app, src, "on-demand")
    assert enrolled() == [seat], (
        "asking for the folder on demand again must write no second row and leave "
        f"the enrolled place untouched; roster={roster(name)!r}"
    )
    assert not app.has_error(), f"the enrol raised an error: {app.error_text()!r}"

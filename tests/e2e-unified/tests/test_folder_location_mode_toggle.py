"""tier_3 — the per-binding on-demand switch (`folder-location-mode-toggle`) on
windows, on linux, and on tui running on a windows or linux host, app-UI driven
(on-demand-files.md § On-Demand Files (Placeholders) / folders.md § Binding).

Windows' on-demand affordance is **location-level**: the switch sits on the bound
row and flips *that binding* between always-resident and cfapi placeholders. It
is the windows leg of the one cross-platform promise `docs/features/files-on-demand.md`
outcome 1 makes — whether a folder is kept in full on this device or fetched on
demand is the user's call per folder, and the app remembers it — whose apple leg
is `test_folder_on_demand_toggle.py` (the set-level `folder-on-demand-toggle`, a
sanctioned deviation: an FP domain cannot take over a user directory, so apple
picks between "bound folder" and "Finder/Files domain" per set, where windows
picks the mode of the bound folder itself). Same promise, two platform
mechanisms, one outcome citing both witnesses — feature-catalog.md § The
coverage contract's "an outcome names what the user gets, never where".

**Why the witness is the page's own re-read, not a disk file.** The mode is
sanctioned device-local config that lives in the sync agent (`LocationConfig`,
persisted in its `config.toml` and pinned there by `bins/fauna-sync-agent`'s own
`config`/`pipe_server` tests — never nest state). This e2e launch runs no live
`fauna-sync-agent`, so — exactly like the sibling nested-binding test
`test_folders.py::test_bind_location_nested_under_folder_windows` — the page is
swapped onto the in-memory location channel (`sync_inject_locations`), which holds
the mode the same way the agent does. What a client-UI test can then verify is the
half the agent's tests cannot: that the flip reaches the channel through the real
page path (`LocationsViewModel.SetModeAsync` → `LocationBindingsController` →
`SetLocationSyncMode`) and that the page **re-reads it from the binding model**
after leaving and re-entering — a toggle holding the choice in surviving view
state passes while the row stays on screen and forgets it on the next visit.

**The linux and tui legs drive the REAL agent.** Both direct-spawn
`fauna-sync-agent` into the launch's isolated world (`drivers/linux.py`,
`drivers/tui.py`), so there the flip goes over the real control plane
(`SetLocationSyncMode` → the agent persists `LocationConfig.mode` and re-drives
its engines, so the placeholder root actually comes up and down — a cfapi root
on a windows host, a FUSE mount over the bound directory on linux) and the
re-entry read comes from the agent's `ListLocations`, mirrored onto the shared
`LocationBindingsModel` row. On a linux host the mount itself is asserted too,
from the kernel's mount table: the flip is not only remembered, it is served. The
per-app difference lives in `SyncLocationsActions.prepare_binding_channel`, not
here. tui renders the switch wherever the shared rule
(`LocationBindingsModel::mode_toggle`, fed by the agent's status reply) says its
agent hosts a placeholder surface — a windows or linux host — so the tui leg runs
on Windows and Linux, and declares its absence on macOS
(`_require_placeholder_host`).

The MUTATION is UI-driven throughout (the toggle click); VERIFICATION reads the
rebuilt row's mode over the uniform `/element/attr?attr=state` contract,
re-entering the page so the read comes from the model rather than view state.
"""

import secrets
import sys
import time

import pytest

from helpers.app_surface import declared_absence
from helpers.folder_content import mountable_location, on_demand_mount_stands

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # `folder-location-mode-toggle` is windows + linux + tui `platform_elements`
    # (ui.yaml § folders: placeholder platforms only — windows via cfapi, linux
    # via the agent's FUSE root, tui wherever its agent has a placeholder
    # surface). macOS/iOS render the set-level `folder-on-demand-toggle` instead,
    # by design rather than by lag, so the other apps are DESELECTED by the
    # marker rather than skipped in-body (convention 7: a skip is not coverage).
    # tui's macOS host is gated in-body with a declared reason
    # (`_require_placeholder_host`).
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.tui,
]


def _require_placeholder_host(app) -> None:
    """tui renders the switch only where its agent hosts a placeholder surface.

    A windows host (cfapi) and a linux host (the agent's FUSE root) run the leg.
    On macOS a bound location is always-resident BY DESIGN — the File Provider
    domain yields to it, and the set-level `folder-on-demand-toggle` is apple's
    switch — so that is a declared absence.
    """
    if not app.driver.is_tui() or sys.platform != "darwin":
        return
    declared_absence(
        app.driver,
        capability="per-binding on-demand switch on a macOS host (a bound location is always-resident)",
        doc=(
            "docs/goal/behavior/on-demand-files.md § On-Demand Files — \"on macOS a "
            "bound location is always-resident by design (§ Apple File Provider binding)\""
        ),
    )


_MODES = {"always", "on-demand"}


def _other(mode: str) -> str:
    return "on-demand" if mode == "always" else "always"


def _fresh_binding_mode(app) -> str:
    """The mode a fresh binding starts in on this host — the agent's
    platform-scoped default, whichever app binds (on-demand-files.md § On-Demand
    Files, *The choice is the user's*): on-demand on a windows host (user ruling
    2026-09-26), always-resident on linux (§ Linux FUSE binding, the flips rule).
    """
    return "on-demand" if sys.platform == "win32" else "always"


def _assert_served(app, src, mode: str) -> None:
    """A linux host only (the linux app, and tui there): the mode is not just
    remembered, it is SERVED — the agent's
    FUSE root stands over the bound directory exactly while the binding is
    on-demand. Read from the kernel's mount table (another process's view),
    polled against a ceiling since the agent mounts after it answers the flip.
    """
    if not (app.driver.is_linux() or (app.driver.is_tui() and sys.platform == "linux")):
        return
    want = mode == "on-demand"
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if on_demand_mount_stands(src) == want:
            return
        time.sleep(0.3)  # sleep-ok: bounded poll cadence inside the deadline, not a settle guess
    pytest.fail(
        f"the binding reads {mode!r} but the on-demand root is "
        f"{'not ' if want else 'still '}mounted over {src}; error={app.error_text()!r}"
    )


def _bind_fresh_folder(app, request, tmp_path) -> tuple[str, str, object]:
    """A fresh owner folder with one local folder bound under it,
    through the page's own add flow. Returns ``(name, scope, src)`` where ``scope``
    is the expanded `folder-row[i]` the nested binding ids resolve inside and
    ``src`` the bound directory.

    `prepare_binding_channel` runs before the first navigate: on windows it seeds
    the in-memory channel EMPTY so `FoldersPage.Page_Loaded` wires the page VM
    onto it (the shape `test_bind_location_nested_under_folder_windows`
    established); on tui it is a no-op — the binding goes to the real agent.
    """
    app.sync_locations.prepare_binding_channel()

    name = f"mode-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    idx = b.find_and_expand_folder(name)
    scope = f"folder-row[{idx}]"

    # Somewhere an on-demand root can be served over (linux: under /tmp).
    src = mountable_location(request, tmp_path, "mode-src")
    # One row expanded at a time, so the unindexed nested add ids resolve to THIS set.
    app.driver.type_text("folder-location-path-input", str(src))
    app.driver.click("folder-location-add-button")

    # Generous ceiling (not an expectation): on tui the add goes to a freshly
    # direct-spawned real agent.
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row", scope=scope) >= 1:
            break
        time.sleep(0.2)  # sleep-ok: bounded poll cadence inside the deadline, not a settle guess
    assert app.driver.count("folder-location-row", scope=scope) >= 1, (
        f"the bound folder did not render nested under {name!r}; error={app.error_text()!r}"
    )
    return name, scope, src


def _re_enter_folder(app, name: str) -> str:
    """Leave the Folders page and come back, re-expanding ``name``'s row so every
    nested read below comes from the page's fresh load of the binding model."""
    app.mail_settings.navigate()
    b = app.backups
    b.navigate_folders()
    # `expand_folder` TOGGLES, and linux keeps a row expanded across a
    # navigation: a single expand there would close the row this test left
    # open. Expand until the row's own binding form is showing instead.
    idx = b.find_and_expand_folder_until(name, "folder-location-path-input")
    return f"folder-row[{idx}]"


@pytest.mark.feature("files-on-demand")
def test_folder_location_mode_toggle_flip_survives_re_entry(logged_in_app, request, tmp_path):
    """The choice is the user's per folder, and it sticks — both directions.

    `on-demand-files.md` § On-Demand Files (Placeholders): "A folder can be marked
    always-resident ... or on-demand", the choice per folder per device, remembered
    by the platform's sync host; on windows the per-binding
    `folder-location-mode-toggle` is that switch (folders.md § Binding).

    The re-entry half is the load-bearing one: `FoldersPage.BuildLocationRow`
    seeds `ToggleSwitch.IsOn` from the row VM and stamps the mode into the
    toggle's HelpText, so a flip that changed only the control (and never reached
    the channel through `SetModeAsync`) reads back correctly while the row stays
    on screen and is forgotten on the next visit. Re-entering forces the read to
    come from the model's reconcile against the channel.

    The default IS asserted: a fresh binding on a windows host starts
    **on-demand** (user ruling 2026-09-26, uniform with apple's default-ON —
    on-demand-files.md § On-Demand Files, *The choice is the user's*), so there
    `before` must read ``"on-demand"`` and the flip proved is on-demand → always
    → on-demand; a fresh linux binding starts **always-resident** (§ Linux FUSE
    binding), so there it is always → on-demand → always, and each step is also
    checked against the kernel's mount table (`_assert_served`). On windows this
    half pins the app's in-memory channel (`InMemoryLocationControlChannel.
    FreshBindingMode`), the agent's own default being pinned agent-side by
    `pipe_server::tests::add_location_starts_a_fresh_path_in_the_platform_default_mode`;
    on linux and tui it reads the real agent's answer — "the default is the
    agent's, whichever app binds".
    """
    app = logged_in_app
    loc = app.sync_locations
    _require_placeholder_host(app)

    name, scope, src = _bind_fresh_folder(app, request, tmp_path)

    assert loc.mode_toggle_visible(scope=scope), (
        "a bound row must render folder-location-mode-toggle on a placeholder host; "
        f"{app.driver.diagnose('folder-location-mode-toggle')} error={app.error_text()!r}"
    )
    before = loc.mode_toggle_state(scope=scope)
    assert before in _MODES, (
        f"the toggle's state must read the binding's mode (always|on-demand); got {before!r}"
    )
    assert before == _fresh_binding_mode(app), (
        f"a fresh binding on this host starts {_fresh_binding_mode(app)!r} "
        f"(on-demand-files.md § On-Demand Files); got {before!r}"
    )
    flipped = _other(before)

    # MUTATION (UI): flip the bound folder's mode.
    after = loc.toggle_mode(scope=scope)
    assert after == flipped, (
        f"flipping the switch must move the row from {before!r} to {flipped!r}; got "
        f"{after!r} error={app.error_text()!r}"
    )
    assert not app.has_error(), f"the mode flip raised an error: {app.error_text()!r}"
    _assert_served(app, src, flipped)

    # VERIFICATION: leave the page and come back — the mode must now be re-read
    # from the binding model's reconcile, not from a surviving ToggleSwitch.
    scope = _re_enter_folder(app, name)
    assert loc.mode_toggle_visible(scope=scope), (
        "the bound row must still render the toggle after re-entry; "
        f"{app.driver.diagnose('folder-location-mode-toggle')}"
    )
    assert loc.mode_toggle_state(scope=scope) == flipped, (
        f"the {flipped!r} choice must survive leaving and re-entering the page — a "
        "flip that does not reach the location channel is forgotten on the next "
        f"visit; got {loc.mode_toggle_state(scope=scope)!r}"
    )

    # And back, so the channel is proven to move in both directions (a
    # write-once bug passes the first half alone).
    assert loc.toggle_mode(scope=scope) == before
    scope = _re_enter_folder(app, name)
    assert loc.mode_toggle_state(scope=scope) == before, (
        "flipping back must also survive re-entry (the channel must take the "
        f"second write, not only the first); got {loc.mode_toggle_state(scope=scope)!r}"
    )
    _assert_served(app, src, before)
    assert not app.has_error(), f"the mode round-trip raised an error: {app.error_text()!r}"

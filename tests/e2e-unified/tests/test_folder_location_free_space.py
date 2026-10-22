"""tier_3 — *Free up space* on a bound on-demand row, over the real agent and a
real FUSE root (`docs/features/files-on-demand.md` outcomes 4 and 5).

A linux desktop offers no *Free up space* verb for a FUSE root and a terminal
offers none anywhere, so there the gesture is the app's: the bound row of an
on-demand location carries one button, `folder-location-free-space-button`,
plus two lines — `folder-location-on-device` (how much of the location is on
this device) and `folder-location-free-space-result` (the latest sweep's
outcome). This module is that gesture's one-seat witness
(on-demand-files.md § The free-up-space gesture where the OS offers none, *The
witness is one seat*):

* **Outcome 4 — freeing keeps the file listed and never deletes it.** bind →
  drop a file → the agent uploads it → flip on-demand (UI) → the FUSE root
  stands → press the verb (UI) → the row reports one file freed and nothing on
  this device; the agent logged the dehydrate; `ls`/`stat` through the mount
  still list the name at its size; the Media page still lists the file; `cat`
  returns the original bytes — which this device no longer held, so they came
  from the nest. That last step is also the first app-level hydrate of a
  cloud-only file a single seat can make.
* **Outcome 5 — a change the nest has not recorded is kept.** Same setup; stop
  the nest, change the file through the mount, press → the row reports one
  file kept, and `cat` returns the CHANGED bytes while the nest is down (they
  can only be local). Start the nest on the same data, wait for the change's
  upload, press again → freed.

**Every assertion is another process's view or the agent's own record**: the
kernel's mount table, a child `ls`/`stat`/`cat`, the engine's `file
dehydrated (Free up space, location sweep)` and `file uploaded` lines on the
agent's stderr, and the row's two lines — which are the agent's reading
mirrored off `ListLocations`, never the app's guess (the verb is never
optimistic). The MUTATIONS — the switch and the button — are the app UI.

tui is the lead app and linux follows it; the windows app renders the same
three ids on its own trickle-down row and is tallied unbuilt here until then. The leg
needs a linux host (the FUSE root); tui on a windows host waits for the
windows agent's own sweep, and on macOS a bound location is always-resident by
design.
"""

import secrets
import sys
import time

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.app_surface import app_name, declared_absence, skip_unbuilt
from helpers.folder_content import (
    SYNC_WINDOW_SECS,
    _log_path,
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
    mountable_location,
    on_demand_mount_stands,
    upload_count,
)
from helpers.fuse_mount import run_through_mount
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # `folder-location-free-space-*` is linux + windows + tui `platform_elements`
    # (ui.yaml § folders). tui is the lead app; the other two are unbuilt debt,
    # tallied in-body (convention 7).
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
]

#: Apps whose leg has landed. Grow this together with the trickle-down rows.
_SUPPORTED_APPS = ("tui", "linux")

#: Where each unbuilt app's leg is captured.
_TRACKED = {
    "windows": "docs/goal/behavior/on-demand-files.md § The free-up-space gesture where the OS offers none (the windows app and agent leg)",
}

# A ceiling, not an expectation: the agent mounts after it has answered the
# flip, restarting the folder's engine in between.
_MOUNT_WINDOW_SECS = 90.0

#: The sweep's own line on the agent's stderr (`fauna-sync-agent`
#: `bridge.rs::sweep_one`) — the engine cannot log it without having freed.
_SWEEP_DEHYDRATED = "file dehydrated (Free up space, location sweep)"

_STRINGS = S.devices.sync_locations


def _require_leg(app) -> None:
    """Skip, with its declared class, every app/host whose leg is not built."""
    name = app_name(app.driver)
    if name not in _SUPPORTED_APPS:
        skip_unbuilt(
            app.driver,
            surface="folder-location-free-space-button",
            detail="this app's Free up space verb has not landed",
            tracked=_TRACKED.get(name, "docs/goal/behavior/on-demand-files.md § The free-up-space gesture where the OS offers none"),
        )
    if sys.platform == "darwin":
        declared_absence(
            app.driver,
            capability="Free up space on a bound location on a macOS host (always-resident)",
            doc=(
                "docs/goal/behavior/on-demand-files.md § The free-up-space gesture where the "
                "OS offers none — \"macOS renders neither (a bound location is always-resident "
                "there)\""
            ),
        )
    if sys.platform == "win32":
        skip_unbuilt(
            app.driver,
            surface="the windows agent's location sweep",
            detail="the windows agent answers FreeLocationSpace unsupported until its sweep lands",
            tracked=_TRACKED["windows"],
        )


def _run(app, folder, *argv: str):
    """One short child process through ``folder``'s mount — bounded even when
    the agent behind the mount is dead (``helpers.fuse_mount``)."""
    return run_through_mount(
        folder, *argv, text=True, diagnose=lambda: agent_diagnosis(app, "owner"),
    )


def _size(n: int) -> str:
    """How the shared formatter renders a sub-KiB size (`fauna_core::format::byte_size`)."""
    assert n < 1024, "keep the fixture files under 1 KiB so the size line is exact"
    return S.size.bytes(value=str(n))


def _freed_line(files: int, size: int) -> str:
    return _STRINGS.free_space_result(files=str(files), size=_size(size))


def _kept_line(freed: int, size: int, kept: int) -> str:
    return _STRINGS.free_space_result_kept(files=str(freed), size=_size(size), kept=str(kept))


def _on_device_prefix(on_device: int) -> str:
    """The standing line up to its denominator — the total counts every tracked
    row, which this test does not pin."""
    template = _STRINGS.free_space_on_device(on_device=_size(on_device), total="\x00")
    return template.split("\x00", 1)[0]


def _sweep_logged(app, filename: str) -> bool:
    try:
        text = app.driver.app_stderr_text()
    except Exception:
        return False
    redacted = _log_path(filename)
    return any(
        _SWEEP_DEHYDRATED in line and redacted in line for line in text.splitlines()
    )


def _on_demand_location(app, request, tmp_path, label: str):
    """A fresh set with a bound location holding one uploaded file, turned
    on-demand through the row's switch with its FUSE root standing.

    Returns ``(folder, scope, name, body)``.
    """
    loc = app.sync_locations
    loc.prepare_binding_channel()
    set_name = f"free-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = mountable_location(request, tmp_path, label)
    idx = bind_location_under_set(app, set_name, folder, seat="owner")
    scope = f"folder-row[{idx}]"

    name = f"kept-{secrets.token_hex(3)}.txt"
    body = f"held on this device {secrets.token_hex(8)}\n"
    atomic_write(folder / name, body)
    await_agent_upload(app, name, seat="owner")

    # Always-resident: the verb is the on-demand row's, and absent here.
    assert loc.mode_toggle_state(scope=scope) == "always", (
        f"a fresh linux binding starts always-resident; got {loc.mode_toggle_state(scope=scope)!r}"
    )
    assert not loc.free_space_visible(scope=scope), (
        "folder-location-free-space-button must be absent on an always-resident row — "
        "every byte there is kept by the user's choice"
    )

    # MUTATION (UI): turn the bound folder on-demand.
    assert loc.toggle_mode(scope=scope) == "on-demand", (
        f"the switch did not move the row on-demand; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    wait_until(
        lambda: on_demand_mount_stands(folder),
        _MOUNT_WINDOW_SECS,
        interval=0.3,
        diagnose=lambda: (
            f"the on-demand root was never mounted over {folder}; error={app.error_text()!r}\n"
            + agent_diagnosis(app, "owner")
        ),
    )
    wait_until(
        lambda: loc.free_space_visible(scope=scope),
        30,
        interval=0.3,
        diagnose=lambda: (
            "the on-demand row never rendered folder-location-free-space-button; "
            f"{app.driver.diagnose('folder-location-free-space-button')} "
            f"error={app.error_text()!r}"
        ),
    )
    assert loc.free_space_state(scope=scope) == "idle"
    assert loc.free_space_result_text(scope=scope) is None, (
        "folder-location-free-space-result must be absent until a sweep has run"
    )
    return folder, scope, name, body


@pytest.mark.feature("files-on-demand")
def test_freeing_keeps_the_file_listed_and_never_deletes_it(logged_in_app, request, tmp_path):
    """Outcome 4: *Free up space* drops the bytes, never the file."""
    app = logged_in_app
    _require_leg(app)
    loc = app.sync_locations
    folder, scope, name, body = _on_demand_location(app, request, tmp_path, "free-space")
    size = len(body.encode())

    # The standing reading before the press: the file is on this device.
    on_device = loc.on_device_text(scope=scope)
    assert on_device is not None and on_device.startswith(_on_device_prefix(size)), (
        f"folder-location-on-device must read {size} bytes on this device before freeing; "
        f"got {on_device!r}"
    )

    # MUTATION (UI): press Free up space.
    line = loc.free_space(_freed_line(1, size), scope=scope)
    assert line == _freed_line(1, size), (
        f"the sweep must report one file freed; the row said {line!r} "
        f"(button state {loc.free_space_state(scope=scope)!r})\n" + agent_diagnosis(app, "owner")
    )
    wait_until(
        lambda: (loc.on_device_text(scope=scope) or "").startswith(_on_device_prefix(0)),
        60,
        interval=0.5,
        diagnose=lambda: (
            "folder-location-on-device never read nothing on this device after the sweep; "
            f"got {loc.on_device_text(scope=scope)!r}"
        ),
    )
    assert _sweep_logged(app, name), (
        f"the agent never logged {_SWEEP_DEHYDRATED!r} for {name!r}\n"
        + agent_diagnosis(app, "owner")
    )

    # VERIFICATION, from other processes: the name stays, at its size.
    listed = _run(app, folder, "ls", "-1", str(folder))
    assert listed.returncode == 0 and name in listed.stdout.split("\n"), (
        f"`ls` through the mount must still list {name!r} after freeing; "
        f"rc={listed.returncode} stdout={listed.stdout!r} stderr={listed.stderr!r}"
    )
    stat = _run(app, folder, "stat", "-c", "%s", str(folder / name))
    assert stat.returncode == 0 and stat.stdout.strip() == str(size), (
        f"a freed file keeps its size in the listing; rc={stat.returncode} "
        f"stdout={stat.stdout!r} stderr={stat.stderr!r}"
    )

    # The nest still holds it: the Media page lists it.
    media = app.media

    def _listed_on_media() -> bool:
        media.reenter()
        return name in media.item_names()

    wait_until(
        _listed_on_media,
        60,
        interval=1.0,
        diagnose=lambda: f"the Media page no longer lists {name!r}; error={app.error_text()!r}",
    )

    # And the bytes come back from it: this device no longer held them.
    read = _run(app, folder, "cat", str(folder / name))
    assert read.returncode == 0 and read.stdout == body, (
        f"`cat` of a freed file must hydrate the original bytes from the nest; "
        f"rc={read.returncode} stdout={read.stdout!r} stderr={read.stderr!r}\n"
        + agent_diagnosis(app, "owner")
    )


@pytest.mark.feature("files-on-demand")
def test_an_unrecorded_change_is_kept(logged_in_app, nest_instance, request, tmp_path):
    """Outcome 5: a change that has not reached the nest cannot have its space freed."""
    app = logged_in_app
    _require_leg(app)
    loc = app.sync_locations
    folder, scope, name, body = _on_demand_location(app, request, tmp_path, "free-kept")
    uploads_before = upload_count(app, name)
    changed = body + f"changed with the nest down {secrets.token_hex(8)}\n"

    stop_nest(nest_instance, graceful=True)
    try:
        # A change through the mount the nest cannot record.
        wrote = _run(
            app, folder, "sh", "-c", 'printf %s "$1" > "$2"', "sh", changed, str(folder / name),
        )
        assert wrote.returncode == 0, (
            f"changing a file through the on-demand mount failed; rc={wrote.returncode} "
            f"stderr={wrote.stderr!r}"
        )

        # MUTATION (UI): press Free up space — the change is the only copy.
        line = loc.free_space(_kept_line(0, 0, 1), scope=scope)
        assert line == _kept_line(0, 0, 1), (
            f"the sweep must keep the unrecorded change; the row said {line!r}\n"
            + agent_diagnosis(app, "owner")
        )
        read = _run(app, folder, "cat", str(folder / name))
        assert read.returncode == 0 and read.stdout == changed, (
            "with the nest down, `cat` must return the CHANGED bytes — they can only be "
            f"local; rc={read.returncode} stdout={read.stdout!r} stderr={read.stderr!r}"
        )
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back on the same data: the change uploads, and then it frees.
    wait_until(
        lambda: upload_count(app, name) > uploads_before,
        SYNC_WINDOW_SECS,
        interval=2.0,
        diagnose=lambda: (
            f"the change to {name!r} was never uploaded after the nest came back "
            f"within {SYNC_WINDOW_SECS:.0f}s; error={app.error_text()!r}\n"
            + agent_diagnosis(app, "owner")
        ),
    )
    size = len(changed.encode())
    line = loc.free_space(_freed_line(1, size), scope=scope)
    assert line == _freed_line(1, size), (
        f"once recorded, the change must free; the row said {line!r}\n"
        + agent_diagnosis(app, "owner")
    )
    read = _run(app, folder, "cat", str(folder / name))
    assert read.returncode == 0 and read.stdout == changed, (
        f"the freed change must hydrate back from the nest; rc={read.returncode} "
        f"stdout={read.stdout!r} stderr={read.stderr!r}"
    )

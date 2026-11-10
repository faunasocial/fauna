"""tier_3 — *Free up space* and the on-demand switch on bound rows, over the
real agent and its real on-demand roots — FUSE on linux, cloud files on
windows (`docs/features/files-on-demand.md` outcomes 4, 5, 6 and 8).

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
* **Outcome 6 — several on-demand folders at once, each switch leaving the
  others working.** Two sets, each with a bound location holding one uploaded
  file. Turning the second on-demand leaves the first's root standing and its
  engine unrestarted; both files are freed, so each is a placeholder only its
  own root can serve. Turning the first back to always → its root comes down
  and its file lands on disk, while the second's root still stands, its engine
  was not restarted, `cat` hydrates its file from the nest and a new file
  written through it uploads (on-demand-files.md § On-Demand Files, *live
  start/stop … without restarting the others*). The freed file stands in for a
  second seat's: it is a file this device does not hold, which one seat can make.
* **Outcome 8 — turning on-demand off keeps an unsent change.** Stop the nest,
  change the file through the mount, turn the switch off → the row reads
  always-resident, the root is down and the directory holds the CHANGED bytes;
  start the nest on the same data → the change uploads, and the file on disk
  is still the change (on-demand-files-linux.md § Linux FUSE binding, *Mode
  flips … on-demand→always: unmount, start the resident engine*).

**Every assertion is another process's view or the agent's own record**: the
kernel's mount table, a child `ls`/`stat`/`cat` (on windows: the filter's
sync-root registration, a file's attributes, a child `cmd /c dir` / `cmd /c
type`), the engine's `file dehydrated (Free up space, location sweep)`, `file
uploaded` and `restarting engine` lines on the agent's log, and the row's two
lines — which are the agent's reading mirrored off `ListLocations`, never the
app's guess (the verb is never optimistic). The MUTATIONS — the switch and the
button — are the app UI.

tui is the lead app; linux and windows render the same three ids. Every outcome
runs on a linux host (tui, linux) over the FUSE root and on a windows host (tui,
windows) over the agent's cloud-files root, where the agent's sweep walks the
rows through the shell verb's per-file gate (on-demand-files.md § The
free-up-space gesture where the OS offers none). There the root standing is the
filter's sync-root registration and a placeholder is the file's own OS
attributes. Outcome 6 frees with Explorer's own *Free up space* on a windows
host — the OS verb the root already honours — and with the app's verb on a
linux host. Every test carries the real-agent markers, without which the
windows app binds into an in-memory channel and runs no agent at all (tui and
linux ignore them). On macOS a bound location is always-resident by design.
"""

import secrets
import sys
import time
from pathlib import Path
from typing import NamedTuple

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.app_surface import (
    app_name,
    require_surface,
    surface_built,
)
from helpers.folder_content import (
    SYNC_WINDOW_SECS,
    _log_path,
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
    engine_restart_count,
    mountable_location,
    on_demand_root_stands,
    open_bound_row,
    upload_count,
)
from helpers.fuse_mount import run_through_mount
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # `folder-location-free-space-*` is linux + windows + tui `platform_elements`
    # (ui.yaml § folders), built on all three (`surfaces.toml`).
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    # On a macOS host a bound location is always-resident by design
    # (on-demand-files.md § The free-up-space gesture where the OS offers none:
    # "macOS renders neither"), so no leg is owed there — tui's included
    # (feature-catalog.md § The ledger, *Stale records*, ruling (i)).
    pytest.mark.host_platform("linux", "windows"),
]

# A ceiling, not an expectation: the agent mounts after it has answered the
# flip, restarting the folder's engine in between.
_MOUNT_WINDOW_SECS = 90.0

#: The sweep's own line on the agent's stderr (`fauna-sync-agent`
#: `bridge.rs::sweep_one`) — the engine cannot log it without having freed.
_SWEEP_DEHYDRATED = "file dehydrated (Free up space, location sweep)"

_STRINGS = S.devices.sync_locations


#: A windows host's ceiling for the pin reaction Explorer's *Free up space*
#: starts in the agent: it rides the watcher's debounce or, missing it, the
#: rescan tick's pin sweep (`test_filesync_mass_delete_floor.py`'s `_STATUS_S`).
_PIN_REACTION_SECS = 150.0

_FREE_SPACE = "folder-location-free-space-button"


def _require_sweep(app) -> None:
    """Outcomes 4 and 5 press the app's verb, so they need it built in the app;
    the host's agent sweeps the location on linux and windows alike."""
    require_surface(app.driver, _FREE_SPACE)


def _run(app, folder, *argv: str):
    """One short child process through ``folder``'s mount — bounded even when
    the agent behind the mount is dead (``helpers.fuse_mount``)."""
    return run_through_mount(
        folder, *argv, text=True, diagnose=lambda: agent_diagnosis(app, "owner"),
    )


def _read(app, folder: Path, path: Path):
    """``path``'s bytes as a child process reads them through ``folder``'s
    root: ``cat`` on linux, ``cmd /c type`` on windows — the other-process read
    `helpers.windows_placeholder.hydrate_via_shell` raises, which a cloud-only
    placeholder answers by fetching from the agent."""
    if sys.platform == "win32":
        return _run(app, folder, "cmd", "/c", "type", str(path))
    return _run(app, folder, "cat", str(path))


def _list(app, folder: Path):
    """``folder``'s names as a child process lists them through its root, one
    per line: ``ls -1`` on linux, ``cmd /c dir /b`` on windows — an enumeration
    the cloud-files root answers from its placeholders, never a hydrate."""
    if sys.platform == "win32":
        return _run(app, folder, "cmd", "/c", "dir", "/b", str(folder))
    return _run(app, folder, "ls", "-1", str(folder))


def _write(app, folder: Path, path: Path, text: str) -> None:
    """Write ``text`` over ``path`` in place, through ``folder``'s root. On linux
    a child does it, bounded like every touch of a FUSE mount. On windows the
    test writes it: a cloud-files root calls its provider only for bytes that
    are not here, never for a write, so nothing can wedge."""
    if sys.platform == "win32":
        path.write_text(text, newline="")
        return
    wrote = _run(app, folder, "sh", "-c", 'printf %s "$1" > "$2"', "sh", text, str(path))
    assert wrote.returncode == 0, (
        f"writing {path.name!r} through the on-demand mount failed; rc={wrote.returncode} "
        f"stderr={wrote.stderr!r}"
    )


def _text_or_none(path: Path) -> str | None:
    """``path``'s text, or ``None`` while it is absent or unreadable. A windows
    root coming down takes its cloud-only placeholders off the disk and the
    agent then downloads each file again, so a read in between can find it
    absent or refused."""
    try:
        return path.read_text()
    except OSError:
        return None


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


class _Location(NamedTuple):
    """One set with one bound location holding one uploaded file."""

    set_name: str
    folder: Path
    scope: str
    name: str
    body: str


def _close_row(app, set_name: str) -> None:
    """Leave ``set_name``'s row shut, so the binding form's unindexed ids
    resolve to the next set the test binds, never this one."""
    b = app.backups
    b.navigate_folders()
    deadline = time.monotonic() + 30
    while True:
        idx = b.wait_for_folder_row(set_name)
        if not app.sync_locations.mode_toggle_visible(scope=f"folder-row[{idx}]"):
            return
        if time.monotonic() >= deadline:
            pytest.fail(f"{set_name!r}'s row would not shut; error={app.error_text()!r}")
        b.expand_folder(idx)


def _bound_location(app, request, tmp_path, label: str) -> _Location:
    """A fresh set with a bound, always-resident location holding one
    uploaded file.

    No `prepare_binding_channel`: on windows it would swap the page onto the
    in-memory channel, and every test that reaches here on the windows app
    carries the real-agent markers, so the bind must reach the real agent
    (tui and linux bind through it anyway)."""
    loc = app.sync_locations
    set_name = f"free-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = mountable_location(request, tmp_path, label)
    idx = bind_location_under_set(app, set_name, folder, seat="owner")
    scope = f"folder-row[{idx}]"
    if sys.platform == "win32":
        # A fresh windows binding starts on-demand (the agent's platform
        # default, on-demand-files.md § On-Demand Files, *The choice is the
        # user's*); always-resident is the deliberate second step through the
        # row's switch, after which the flow is every host's.
        fresh = loc.mode_toggle_state(scope=scope)
        assert fresh == "on-demand", (
            f"a fresh windows binding starts on-demand; got {fresh!r} "
            f"error={app.error_text()!r}"
        )
        scope = _turn_always(app, _Location(set_name, folder, scope, "", ""))

    name = f"kept-{secrets.token_hex(3)}.txt"
    body = f"held on this device {secrets.token_hex(8)}\n"
    # Bytes, not text: a text write turns the newline into CRLF on windows,
    # and every size this module checks is the body's own.
    atomic_write(folder / name, body.encode())
    await_agent_upload(app, name, seat="owner")

    # Always-resident: the verb is the on-demand row's, and absent here. The
    # visibility read scrolls the row in first — by outcome 6 the module's sets
    # put this row past the fold, where the state read answers `None`.
    assert loc.mode_toggle_visible(scope=scope), (
        "the bound row must render folder-location-mode-toggle; "
        f"{app.driver.diagnose('folder-location-mode-toggle')} error={app.error_text()!r}"
    )
    assert loc.mode_toggle_state(scope=scope) == "always", (
        f"the binding must read always-resident here; got {loc.mode_toggle_state(scope=scope)!r}"
    )
    assert not loc.free_space_visible(scope=scope), (
        "folder-location-free-space-button must be absent on an always-resident row — "
        "every byte there is kept by the user's choice"
    )
    return _Location(set_name, folder, scope, name, body)


def _turn_on_demand(app, location: _Location) -> None:
    """MUTATION (UI): flip the row's switch on-demand, then wait for its root
    to stand and — in an app that has built it (`surfaces.toml`) — the row to
    offer *Free up space*, idle and with no outcome yet."""
    loc = app.sync_locations
    scope = location.scope
    assert loc.toggle_mode(scope=scope) == "on-demand", (
        f"the switch did not move the row on-demand; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    wait_until(
        lambda: on_demand_root_stands(location.folder),
        _MOUNT_WINDOW_SECS,
        interval=0.3,
        diagnose=lambda: (
            f"the on-demand root never stood over {location.folder}; "
            f"error={app.error_text()!r}\n" + agent_diagnosis(app, "owner")
        ),
    )
    if not surface_built(_FREE_SPACE, app_name(app.driver)):
        return
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


def _turn_always(app, location: _Location) -> str:
    """MUTATION (UI): flip the row's switch back to always-resident, wait for
    its root to come down, and return the row's scope."""
    loc = app.sync_locations
    scope = open_bound_row(app, location.set_name)
    assert loc.toggle_mode(scope=scope) == "always", (
        f"the switch did not move the row back to always; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    wait_until(
        lambda: not on_demand_root_stands(location.folder),
        _MOUNT_WINDOW_SECS,
        interval=0.3,
        diagnose=lambda: (
            f"the on-demand root never came down from {location.folder}; "
            f"error={app.error_text()!r}\n" + agent_diagnosis(app, "owner")
        ),
    )
    return scope


def _on_demand_location(app, request, tmp_path, label: str) -> _Location:
    """A fresh set with a bound location holding one uploaded file, turned
    on-demand through the row's switch with its root standing."""
    location = _bound_location(app, request, tmp_path, label)
    _turn_on_demand(app, location)
    return location


@pytest.mark.feature("files-on-demand")
# The windows app binds into the real agent only under BOTH real-agent markers
# (conftest `_apply_isolated_sync_agent_env`); tui and linux ignore them.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_freeing_keeps_the_file_listed_and_never_deletes_it(logged_in_app, request, tmp_path):
    """Outcome 4: *Free up space* drops the bytes, never the file."""
    app = logged_in_app
    _require_sweep(app)
    loc = app.sync_locations
    _, folder, scope, name, body = _on_demand_location(app, request, tmp_path, "free-space")
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
    listed = _list(app, folder)
    assert listed.returncode == 0 and name in listed.stdout.splitlines(), (
        f"a listing through the root must still show {name!r} after freeing; "
        f"rc={listed.returncode} stdout={listed.stdout!r} stderr={listed.stderr!r}"
    )
    if sys.platform == "win32":
        # The file's own OS attributes and a metadata lookup: neither opens the
        # data, so neither hydrates what the sweep just freed.
        from helpers.windows_placeholder import describe

        assert _windows_placeholder(folder / name, size), (
            f"a freed file is a cloud-only placeholder at its size; "
            f"{describe(str(folder / name))}"
        )
    else:
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
    read = _read(app, folder, folder / name)
    assert read.returncode == 0 and read.stdout == body, (
        f"reading a freed file must hydrate the original bytes from the nest; "
        f"rc={read.returncode} stdout={read.stdout!r} stderr={read.stderr!r}\n"
        + agent_diagnosis(app, "owner")
    )


@pytest.mark.feature("files-on-demand")
# BOTH real-agent markers, as outcome 4 above.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_an_unrecorded_change_is_kept(logged_in_app, nest_instance, request, tmp_path):
    """Outcome 5: a change that has not reached the nest cannot have its space freed."""
    app = logged_in_app
    _require_sweep(app)
    loc = app.sync_locations
    _, folder, scope, name, body = _on_demand_location(app, request, tmp_path, "free-kept")
    uploads_before = upload_count(app, name)
    changed = body + f"changed with the nest down {secrets.token_hex(8)}\n"

    stop_nest(nest_instance, graceful=True)
    try:
        # A change through the root the nest cannot record.
        _write(app, folder, folder / name, changed)

        # MUTATION (UI): press Free up space — the change is the only copy.
        line = loc.free_space(_kept_line(0, 0, 1), scope=scope)
        assert line == _kept_line(0, 0, 1), (
            f"the sweep must keep the unrecorded change; the row said {line!r}\n"
            + agent_diagnosis(app, "owner")
        )
        read = _read(app, folder, folder / name)
        assert read.returncode == 0 and read.stdout == changed, (
            "with the nest down, a read must return the CHANGED bytes — they can only be "
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
    read = _read(app, folder, folder / name)
    assert read.returncode == 0 and read.stdout == changed, (
        f"the freed change must hydrate back from the nest; rc={read.returncode} "
        f"stdout={read.stdout!r} stderr={read.stderr!r}"
    )


def _size_and_blocks(app, folder: Path, name: str) -> tuple[int, int] | None:
    """``(st_size, st_blocks)`` of ``name`` as another process stats it through
    ``folder``'s mount — ``None`` while the view does not list it. A ``stat``
    is a lookup, never an open, so it does not hydrate; zero blocks is the root's
    own answer for a placeholder (`test_folder_on_demand_hydrate_linux.py`)."""
    out = _run(app, folder, "stat", "-c", "%s %b", str(folder / name))
    if out.returncode != 0:
        return None
    size, blocks = out.stdout.split()
    return int(size), int(blocks)


def _windows_placeholder(path: Path, size: int) -> bool:
    """Whether ``path`` is a cloud-only placeholder of ``size`` bytes, read from
    its OS attributes and a metadata lookup — neither opens the data, so
    neither hydrates (`helpers.windows_placeholder`)."""
    from helpers.windows_placeholder import is_placeholder

    try:
        return is_placeholder(str(path)) and path.stat().st_size == size
    except FileNotFoundError:
        return False


def _free_to_placeholder(app, location: _Location) -> None:
    """MUTATION: free the location's one file, then wait until another process
    sees it listed at its size with no bytes on this device.

    On linux the gesture is the app's verb (UI). On windows it is Explorer's
    *Free up space* — the OS verb the cloud-files root honours, an attribute
    write the agent's pin reaction answers by freeing the bytes
    (`helpers.windows_cfapi.free_up_space`). Outcome 6 keeps that OS road on
    windows so the module witnesses both: outcomes 4 and 5 press the app's."""
    size = len(location.body.encode())
    if sys.platform == "win32":
        from helpers.windows_cfapi import free_up_space
        from helpers.windows_placeholder import describe

        path = location.folder / location.name
        free_up_space(path)
        wait_until(
            lambda: _windows_placeholder(path, size),
            _PIN_REACTION_SECS,
            interval=0.5,
            diagnose=lambda: (
                f"{location.name!r} never became a placeholder of {size} bytes after "
                f"Explorer's Free up space; {describe(str(path))}\n"
                + agent_diagnosis(app, "owner")
            ),
        )
        return
    loc = app.sync_locations
    scope = open_bound_row(app, location.set_name)
    line = loc.free_space(_freed_line(1, size), scope=scope)
    assert line == _freed_line(1, size), (
        f"the sweep over {location.set_name!r} must report one file freed; the row said "
        f"{line!r}\n" + agent_diagnosis(app, "owner")
    )
    # Polled: the kernel's attribute cache may still hold the hydrated answer.
    wait_until(
        lambda: _size_and_blocks(app, location.folder, location.name) == (size, 0),
        60,
        interval=0.5,
        diagnose=lambda: (
            f"{location.name!r} never listed as a placeholder (size {size}, no blocks) "
            f"after freeing; `stat` reads "
            f"{_size_and_blocks(app, location.folder, location.name)!r}"
        ),
    )


@pytest.mark.feature("files-on-demand")
# The windows app binds into the real agent only under BOTH real-agent markers
# (conftest `_apply_isolated_sync_agent_env`); tui and linux ignore them.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_several_on_demand_folders_each_switch_leaves_the_others_working(
    logged_in_app, request, tmp_path
):
    """Outcome 6: several folders on demand at once; a switch drives only its own.

    No `folder-location-free-space-button` gate: the catalog reads a test's
    surfaces the same on every host, and here the button is needed only where
    the app's verb frees — a linux host. On a windows host Explorer's verb
    frees (`_free_to_placeholder`)."""
    app = logged_in_app
    first = _on_demand_location(app, request, tmp_path, "multi-first")
    _close_row(app, first.set_name)
    second = _bound_location(app, request, tmp_path, "multi-second")

    # Turning the second ON leaves the first's root standing and its engine
    # unrestarted. The baseline is taken after the second's bind, which is a
    # reconcile of its own.
    first_restarts = engine_restart_count(app, first.set_name)
    _turn_on_demand(app, second)
    assert on_demand_root_stands(first.folder), (
        f"turning {second.set_name!r} on-demand took down {first.set_name!r}'s root\n"
        + agent_diagnosis(app, "owner")
    )
    assert engine_restart_count(app, first.set_name) == first_restarts, (
        f"turning {second.set_name!r} on-demand restarted {first.set_name!r}'s engine\n"
        + agent_diagnosis(app, "owner")
    )

    # Both files freed: each is now a file this device does not hold, which
    # only its own root can serve.
    _free_to_placeholder(app, first)
    _free_to_placeholder(app, second)

    # Turning the first OFF: its root comes down and its file lands on disk —
    # downloaded and kept (on-demand-files.md § The free-up-space gesture, *Keep
    # on this device is the switch*).
    second_restarts = engine_restart_count(app, second.set_name)
    _turn_always(app, first)
    wait_until(
        lambda: _text_or_none(first.folder / first.name) == first.body,
        SYNC_WINDOW_SECS,
        interval=1.0,
        diagnose=lambda: (
            f"{first.name!r} never landed on disk after {first.set_name!r} went back to "
            f"always; the directory holds {sorted(p.name for p in first.folder.iterdir())!r}\n"
            + agent_diagnosis(app, "owner")
        ),
    )

    # …and the second kept working: its root stands, its engine was not
    # restarted, it serves its placeholder from the nest and uploads a new file.
    assert on_demand_root_stands(second.folder), (
        f"turning {first.set_name!r} back to always took down {second.set_name!r}'s root\n"
        + agent_diagnosis(app, "owner")
    )
    assert engine_restart_count(app, second.set_name) == second_restarts, (
        f"turning {first.set_name!r} back to always restarted {second.set_name!r}'s engine\n"
        + agent_diagnosis(app, "owner")
    )
    read = _read(app, second.folder, second.folder / second.name)
    assert read.returncode == 0 and read.stdout == second.body, (
        f"reading {second.name!r} must hydrate it from the nest through the root that "
        f"kept running; rc={read.returncode} stdout={read.stdout!r} stderr={read.stderr!r}\n"
        + agent_diagnosis(app, "owner")
    )
    added = f"added-{secrets.token_hex(3)}.txt"
    _write(
        app, second.folder, second.folder / added,
        f"written after the other folder's flip {secrets.token_hex(8)}\n",
    )
    await_agent_upload(app, added, seat="owner")
    assert not app.has_error(), f"the flips surfaced an error: {app.error_text()!r}"


@pytest.mark.feature("files-on-demand")
# BOTH real-agent markers, as outcome 6 above.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_turning_on_demand_off_keeps_an_unsent_change(
    logged_in_app, nest_instance, request, tmp_path
):
    """Outcome 8: switching on-demand off never destroys a change not yet uploaded."""
    app = logged_in_app
    loc = app.sync_locations
    location = _on_demand_location(app, request, tmp_path, "off-kept")
    folder, name = location.folder, location.name
    uploads_before = upload_count(app, name)
    changed = location.body + f"changed with the nest down {secrets.token_hex(8)}\n"

    stop_nest(nest_instance, graceful=True)
    try:
        # A change through the root the nest cannot record.
        _write(app, folder, folder / name, changed)

        # MUTATION (UI): turn on-demand off — the change is the only copy.
        scope = _turn_always(app, location)
        assert not loc.free_space_visible(scope=scope), (
            "folder-location-free-space-button must be gone once the row is always-resident"
        )
        # The root is down, so this is the plain directory every process reads.
        assert (folder / name).read_text() == changed, (
            "turning on-demand off must leave the CHANGED bytes on disk; the file reads "
            f"{(folder / name).read_text()!r}\n" + agent_diagnosis(app, "owner")
        )
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back on the same data: the change uploads. The ceiling covers
    # the resident engine's build retry — it was refused with the nest down and
    # retries after the agent's idle cadence (`engine_driver.rs::build_until_served`).
    wait_until(
        lambda: upload_count(app, name) > uploads_before,
        2 * SYNC_WINDOW_SECS,
        interval=2.0,
        diagnose=lambda: (
            f"the change to {name!r} was never uploaded after the nest came back "
            f"within {2 * SYNC_WINDOW_SECS:.0f}s; error={app.error_text()!r}\n"
            + agent_diagnosis(app, "owner")
        ),
    )
    assert (folder / name).read_text() == changed, (
        "the change must still be on disk after it uploaded — the resident engine never "
        f"rolls it back to the nest's older version; reads {(folder / name).read_text()!r}"
    )

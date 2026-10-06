"""tier_3 — a linux on-demand folder is served to OTHER processes at its own path.

The linux leg of `docs/features/files-on-demand.md` beyond the switch itself
(`test_folder_location_mode_toggle.py`): once the user turns a bound folder
on-demand on the Folders page, the per-user `fauna-sync-agent` mounts a FUSE
root over that directory (on-demand-files.md § Linux FUSE binding — mount-over:
the bound directory IS the backing store and the mount is a view of it), and
everything else on the box — a file manager, an editor, `ls`, `cat` — reads and
writes the folder through that view. This module drives that end to end over
the REAL agent the linux e2e launch direct-spawns:

    bind (UI) → drop a file → the agent uploads it
      → flip to on-demand (UI) → the kernel's mount table shows `fuse.fauna`
      → `ls` and `cat` FROM A SUBPROCESS list the file and return its bytes
      → a subprocess WRITES a new file through the mount → the agent uploads it
      → a subprocess CHANGES the first file through the mount → uploaded again
      → flip back (UI) → the mount is gone and the directory is an ordinary
        directory holding both files.

**Every assertion is another process's view, never the app's or the agent's
claim**: the mount table, a child `ls`/`cat`, and — for "the file is synced" —
the engine's own `file uploaded` line on the agent's stderr, which it cannot
emit without the byte plane having accepted the file. Linux renders no per-file
sync badge (the Media page reads every file as synced), so the engine's record
is the honest witness of `Synced`, not a UI element.

**What this does NOT prove, and where it is proved.** Hydrating a *cloud-only*
placeholder needs a file the device does not hold, which one seat cannot
produce through the app UI (linux has no "free up space" gesture). That
mechanism — open hydrates, flips the row `Synced`, a dehydrated file is never
a delete — is pinned live against a real mount by the agent's own
`fuse_live_integration` suite (the `fuse-live-test-check` merge gate). This
module pins what that suite cannot: that the APP's switch reaches a real agent
and the folder the user sees is the one being served.

The bound directory sits under `/tmp` whatever `TMPDIR` says
(`helpers.folder_content.mountable_location`): Ubuntu's AppArmor profile for
`fusermount3` admits a mount point only under the user's home, `/mnt`,
`/media`, `/run/user/<uid>` or `/tmp`.
"""

import secrets
import subprocess
import time

import pytest

from helpers.folder_content import (
    SYNC_WINDOW_SECS,
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
    mountable_location,
    on_demand_mount_stands,
    upload_count,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

# A ceiling, not an expectation: the agent mounts (or unmounts) after it has
# answered the flip, restarting the folder's engine in between.
_MOUNT_WINDOW_SECS = 90.0


def _await_mount(app, location, *, mounted: bool) -> None:
    """Poll the kernel's mount table until the on-demand root is (or is no
    longer) mounted over ``location`` — a state, read from outside the agent."""
    deadline = time.monotonic() + _MOUNT_WINDOW_SECS
    while time.monotonic() < deadline:
        if on_demand_mount_stands(location) == mounted:
            return
        time.sleep(0.3)  # sleep-ok: bounded poll cadence inside the deadline, not a settle guess
    pytest.fail(
        f"the on-demand root was {'never mounted over' if mounted else 'never unmounted from'} "
        f"{location} within {_MOUNT_WINDOW_SECS:.0f}s; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )


def _run(*argv: str) -> subprocess.CompletedProcess:
    """One short child process — the 'another process' of every assertion.
    Bounded: a hung FUSE request must fail the test, never wedge the run."""
    return subprocess.run(argv, capture_output=True, text=True, timeout=120)


@pytest.mark.parametrize("folder_share_owner_app", ["linux"], indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("files-on-demand")
def test_on_demand_folder_is_read_and_written_through_its_mount(
    folder_share_owner_app, request, tmp_path
):
    """Turning a bound folder on-demand serves it, at its own path, to every
    other process — and turning it off leaves an ordinary directory behind."""
    app, _nest, _owner = folder_share_owner_app
    loc = app.sync_locations

    set_name = f"ondemand-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = mountable_location(request, tmp_path, "on-demand-read")
    idx = bind_location_under_set(app, set_name, folder, seat="owner")
    scope = f"folder-row[{idx}]"

    # A file the folder holds before the flip — uploaded by the resident engine.
    first = f"before-{secrets.token_hex(3)}.txt"
    first_body = f"kept on this device {secrets.token_hex(8)}\n"
    atomic_write(folder / first, first_body)
    await_agent_upload(app, first, seat="owner")

    # MUTATION (UI): turn the bound folder on-demand.
    assert loc.mode_toggle_visible(scope=scope), (
        "the bound row must render folder-location-mode-toggle; "
        f"{app.driver.diagnose('folder-location-mode-toggle')} error={app.error_text()!r}"
    )
    assert loc.mode_toggle_state(scope=scope) == "always", (
        "a fresh linux binding starts always-resident (on-demand-files.md § Linux "
        f"FUSE binding); got {loc.mode_toggle_state(scope=scope)!r}"
    )
    assert loc.toggle_mode(scope=scope) == "on-demand", (
        f"the switch did not move the row on-demand; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    _await_mount(app, folder, mounted=True)

    # VERIFICATION, from other processes: the view lists the file and serves
    # its bytes.
    listed = _run("ls", "-1", str(folder))
    assert listed.returncode == 0 and first in listed.stdout.split("\n"), (
        f"`ls` through the on-demand mount must list {first!r}; rc={listed.returncode} "
        f"stdout={listed.stdout!r} stderr={listed.stderr!r}"
    )
    read = _run("cat", str(folder / first))
    assert read.returncode == 0 and read.stdout == first_body, (
        f"`cat` through the on-demand mount must return the file's bytes; "
        f"rc={read.returncode} stdout={read.stdout!r} stderr={read.stderr!r}"
    )

    # A write by another process, through the mount, is an ordinary local edit:
    # it lands in the directory under the view and the agent uploads it.
    second = f"through-{secrets.token_hex(3)}.txt"
    second_body = f"written through the mount {secrets.token_hex(8)}\n"
    wrote = _run("sh", "-c", 'printf %s "$1" > "$2"', "sh", second_body, str(folder / second))
    assert wrote.returncode == 0, (
        f"a write through the on-demand mount failed; rc={wrote.returncode} "
        f"stderr={wrote.stderr!r}"
    )
    await_agent_upload(app, second, seat="owner")

    # A CHANGE to a tracked file through the mount uploads like any other
    # (on-demand-files.md § Sync direction — on-demand is a storage choice,
    # never a direction choice). The engine has already logged one upload of
    # this path, so the witness is a NEW upload line, counted, not a re-read
    # of the old one.
    uploads_before = upload_count(app, first)
    changed_body = first_body + f"changed through the mount {secrets.token_hex(8)}\n"
    changed = _run("sh", "-c", 'printf %s "$1" > "$2"', "sh", changed_body, str(folder / first))
    assert changed.returncode == 0, (
        f"changing a file through the on-demand mount failed; rc={changed.returncode} "
        f"stderr={changed.stderr!r}"
    )
    deadline = time.monotonic() + SYNC_WINDOW_SECS
    while time.monotonic() < deadline and upload_count(app, first) <= uploads_before:
        time.sleep(2.0)  # sleep-ok: bounded poll cadence inside the deadline, not a settle guess
    assert upload_count(app, first) > uploads_before, (
        f"a change made to {first!r} through the on-demand mount was never uploaded "
        f"within {SYNC_WINDOW_SECS:.0f}s; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    first_body = changed_body

    # MUTATION (UI): and back. The mount comes down and nothing moved — the
    # directory is an ordinary directory holding both files.
    assert loc.toggle_mode(scope=scope) == "always", (
        f"the switch did not move the row back to always; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    _await_mount(app, folder, mounted=False)
    assert (folder / first).read_text() == first_body
    assert (folder / second).read_text() == second_body
    assert not app.has_error(), f"the round trip surfaced an error: {app.error_text()!r}"

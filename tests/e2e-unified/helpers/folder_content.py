"""Shared choreography for driving REAL shared-folder **content** through a
GUI app's sync engine — the owner-side half every content-level shared-set
e2e needs.

Lifted verbatim (behavior-preserving) out of ``tests/test_folder_agent_content_
sync.py`` when the Phase-0 member-read e2e needed the same owner-side moves
(priority #2 — one copy, not two): bind a location under a set's expander, drop a
file the engine will chunk/seal/upload, and wait on the engine's own log line
that the byte plane accepted it.

Why these live together: all three are the *same* witness chain. The engine is
the only thing that seals a shared set's chunks under the M2 content key
(``MediaMachine::upload`` seals under the uploader's own ``BackupKey`` — see its
``do_upload``), so any test that needs member-decryptable content in a set must
drive a real bound location, and any such test needs the same "did it actually
upload" evidence and the same diagnosis dump when it did not.
"""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

import pytest

from helpers.app_surface import declared_absence, skip_environment
from helpers.fuse_mount import mount_type

# Content sync crosses two processes and a nest round-trip; the engine is
# watcher-driven but the upload adds latency. Bounded always, unbounded never
# (`testing.md` § point 9).
SYNC_WINDOW_SECS = 120.0


# Lines that establish a PRECONDITION — a state set (or refused) early whose
# only consequence appears much later. These are kept wherever they occur in the
# run, never trimmed by the positional tail below.
#
# ⚠ Why this class exists at all (measured 2026-08-22, the share-plane journey).
# A positional tail is structurally blind to a precondition. The filter here
# matches nearly every engine and agent line, so `lines[-80:]` is roughly ONE
# MINUTE of a 17-minute journey — and the phase that decided the outcome has
# long since scrolled off the top. Chasing "is the writer roster ever cached
# while we are still online?" through that tail returned only stamps from the
# final nest-down minute, where not caching is CORRECT, so the online phase
# stayed dark no matter how many times the journey failed. Widening the tail
# alone does not fix that — it just buys minutes against a run whose length is
# unbounded. Keeping the marker CLASS regardless of position does.
PRECONDITION_MARKERS = (
    "share writer roster",   # the peer-accept precondition: cached vs skipped
    "share pump:",           # per-pass refusals/admission — why rows moved or did not
    "share plane:",          # the advertise/offline-hold decisions
    "control plane",         # the Connected transition every refresh is gated on
    # WHICH connection the plane's writes ride, and whether it came up. Settled
    # in the first seconds of a run and never mentioned again, so a positional
    # tail can never reach it — while the failure it explains (the dial row's
    # `GenerationTip` mint, whose escrow deposit is the plane's only
    # synchronous nest call) surfaces minutes later and continuously.
    "principal",
    # The grant leg that decides whether that connection can EVER authenticate
    # — likewise settled early and never mentioned again.
    "enrollment:",
)
# Bounds, so a pathological run cannot turn a diagnosis into a log dump.
PRECONDITION_CAP = 120
TAIL_CAP = 80


def agent_diagnosis(app, seat: str) -> str:
    """The app's captured stderr tail — which on linux also carries the REAL
    ``fauna-sync-agent``'s output, because the app direct-spawns it as a child
    on the same fd. Without this an upload/hydration failure reads as a blank
    "nothing happened"; with it the engine's own refusal is right there
    (``testing.md`` § conventions point 6: failures must diagnose themselves).

    Two sections, because they answer different questions: the precondition
    markers (whole run — see :data:`PRECONDITION_MARKERS` for why position must
    not trim them) say what state the run was in *before* the failing step, and
    the filtered tail says what happened *at* it.
    """
    try:
        text = app.driver.app_stderr_text()
    except Exception as exc:  # pragma: no cover - diagnostic path only
        return f"  [{seat}] app/agent stderr unavailable: {exc!r}"
    lines = [
        ln for ln in text.splitlines()
        if any(k in ln for k in ("sync", "engine", "agent", "capab", "content_key",
                                 "share", "dial", "advert", "principal",
                                 "ERROR", "error", "WARN", "warn", "panic"))
    ]
    pre_idx = {
        i for i, ln in enumerate(lines)
        if any(m in ln for m in PRECONDITION_MARKERS)
    }
    # Oldest-trimmed, so the EARLIEST surviving precondition is still the
    # earliest one shown — a precondition list that dropped its head would
    # reintroduce exactly the blindness this exists to remove.
    pre = [lines[i] for i in sorted(pre_idx)][-PRECONDITION_CAP:]
    rest = [ln for i, ln in enumerate(lines) if i not in pre_idx][-TAIL_CAP:]

    out = []
    if pre:
        body = "\n".join(f"    {ln}" for ln in pre)
        out.append(f"  [{seat}] app+agent stderr — precondition markers (whole run):\n{body}")
    tail = "\n".join(f"    {ln}" for ln in rest) or "    <no matching lines>"
    out.append(f"  [{seat}] app+agent stderr (filtered tail):\n{tail}")
    out.append(preserve_agent_stderr(text, seat))
    return "\n".join(out)


def preserve_agent_stderr(text: str, seat: str) -> str:
    """Save one seat's COMPLETE app+agent stderr somewhere that outlives the
    run, and return a one-line pointer to it.

    Everything above this is a *filter*: the keyword pass drops lines it does
    not recognise and the two caps drop the rest, which is right for a failure
    message but means the raw log exists only for as long as the assertion is
    being formatted. The app's own tmp dir — ``app.err`` included — is gone at
    teardown, so once the run ends the unfiltered text is unrecoverable, and a
    question the filter did not anticipate cannot be asked at all without
    paying for another whole run.

    That is not hypothetical: this journey has spent runs re-deriving state it
    had already logged, because each new question needed a marker the previous
    run's filter did not carry.
    Saving the raw text costs nothing on a green run (this is only ever called
    from a failure diagnosis) and turns the next such question into a grep.

    Best-effort by construction: a diagnosis that can fail is a diagnosis that
    disappears exactly when it is needed, so every error here degrades to a
    note in the message rather than replacing the failure with its own.
    """
    try:
        base = os.environ.get("FAUNA_E2E_DIAGNOSTIC_DIR")
        # `mkdtemp` deliberately, not `TemporaryDirectory`: nothing may clean
        # this up, and the OS temp sweeper is a slower clock than the question
        # it exists to answer.
        target = Path(base) if base else Path(tempfile.mkdtemp(prefix="fauna-e2e-diag-"))
        target.mkdir(parents=True, exist_ok=True)
        path = target / f"{seat}-stderr.log"
        path.write_text(text, encoding="utf-8", errors="replace")
        return (
            f"  [{seat}] COMPLETE unfiltered app+agent stderr preserved at: {path} "
            f"({len(text.splitlines())} lines) — grep it for anything the filter above dropped"
        )
    except Exception as exc:  # pragma: no cover - diagnostic path only
        return f"  [{seat}] complete stderr could not be preserved: {exc!r}"


def atomic_write(path: Path, content: bytes | str) -> None:
    """Write via temp+rename so the engine's watcher never observes a
    half-written body (the multiseat suite's convention)."""
    tmp = path.with_name(path.name + ".tmp-agentsync")
    if isinstance(content, str):
        tmp.write_text(content)
    else:
        tmp.write_bytes(content)
    os.replace(tmp, path)


def mountable_location(request, tmp_path: Path, name: str) -> Path:
    """A fresh directory an **on-demand** binding can be served over.

    On linux the on-demand root is a FUSE mount over the bound directory, and a
    confined ``fusermount3`` (Ubuntu's AppArmor profile) admits a mount point
    only under the user's home, ``/mnt``, ``/media``, ``/run/user/<uid>`` or
    ``/tmp`` (on-demand-files-linux.md § Linux FUSE binding, the lifecycle rule). The
    dev machines point ``TMPDIR`` at a larger volume outside that set, so
    ``tmp_path`` is exactly the location whose mount is refused — hence a
    directory under ``/tmp`` itself, whatever ``TMPDIR`` says (the agent's own
    live harness does the same). Everywhere else ``tmp_path`` is fine.

    Removed at teardown unless a mount still stands over it: deleting through
    a live on-demand view is a user delete of every file in the set.
    """
    if not sys.platform.startswith("linux"):
        location = tmp_path / name
        location.mkdir()
        return location
    location = Path(tempfile.mkdtemp(prefix=f"fauna-e2e-{name}-", dir="/tmp"))

    def _cleanup() -> None:
        if not on_demand_mount_stands(location):
            shutil.rmtree(location, ignore_errors=True)

    request.addfinalizer(_cleanup)
    return location


def on_demand_mount_stands(location: Path) -> bool:
    """Whether the agent's FUSE on-demand root is mounted over ``location``
    right now — read from the kernel's own mount table, so it is the answer
    every other process on the box gets, not the app's or the agent's claim.
    ``fuse.fauna`` is the root's filesystem type (``subtype=fauna``)."""
    return mount_type(location) == "fuse.fauna"


def on_demand_root_stands(location: Path) -> bool:
    """Whether the agent's on-demand root stands over ``location`` right now,
    on whichever host this is: on linux the FUSE mount in the kernel's mount
    table (:func:`on_demand_mount_stands`), on windows the cloud-files sync-root
    registration the filter answers (`helpers.windows_cfapi.is_sync_root`). Each
    is the answer every other process on the box gets — never the app's or the
    agent's claim."""
    if sys.platform == "win32":
        from helpers.windows_cfapi import is_sync_root

        return is_sync_root(location)
    return on_demand_mount_stands(location)


def require_fuse_host(driver) -> None:
    """Skip, with its class, a witness that reads the linux FUSE root on a host
    that serves none — tui runs on all three. ``driver`` is a driver or the app id.

    A macOS host offers no per-binding on-demand root by design; a windows host
    serves a cloud-files root, which the FUSE witnesses' mount table and block
    counts cannot read (its legs are `test_folder_location_free_space.py` and
    the `test_folder_on_demand_*_windows.py` twins).
    """
    if sys.platform == "darwin":
        declared_absence(
            driver,
            capability="on-demand root over a bound location on a macOS host (always-resident)",
            doc=(
                "docs/goal/behavior/on-demand-files.md § On-Demand Files — \"on macOS a "
                "bound location is always-resident by design (on-demand-files-apple.md § Apple File Provider binding)\""
            ),
        )
    if sys.platform == "win32":
        skip_environment("this witness reads a FUSE mount; a windows host serves cloud files")


def bind_location_under_set(app, set_name: str, location: Path, *, seat: str) -> int:
    """Bind ``location`` under ``set_name``'s expander and wait for the row.

    The binding form is nested per set (the set is contextual — there is no
    free-text set name), so the unindexed ``folder-location-*`` IDs resolve to the
    one expanded set.

    The open, the type and the click are retried until the form takes them,
    polled on the form being there (convention 14): linux rebuilds the whole
    folder list on every devices snapshot, collapsing the open row, and on an
    account carrying many sets — the run-scoped user late in a whole run, 54
    sets measured  — the member-roster loads keep the snapshots
    coming for seconds after the page mounts, so a single open-then-type lost
    the form to a rebuild (``LookupError: not found`` on the path input) while
    the same cell passed alone. The wait for the bound row re-opens the row for
    the same reason.
    """
    b = app.backups
    b.navigate_folders()
    deadline = time.monotonic() + 60
    while True:
        # The row may already be open (the owner's, from the share gesture).
        idx = b.open_folder(set_name)
        try:
            app.driver.type_text("folder-location-path-input", str(location))
            app.driver.click("folder-location-add-button")
            break
        except LookupError:
            if time.monotonic() >= deadline:
                pytest.fail(
                    f"[{seat}] {set_name!r}'s binding form never held still long "
                    f"enough to take the path (60s of re-opens); "
                    f"error={app.error_text()!r}"
                )
            time.sleep(0.3)  # sleep-ok: bounded poll cadence inside the deadline

    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
            return idx
        if not app.driver.is_visible("folder-location-path-input"):
            # A rebuild shut the row after the add: open it again to read it.
            idx = b.open_folder(set_name)
        time.sleep(0.3)
    pytest.fail(
        f"[{seat}] bound location never rendered as a folder-location-row under "
        f"{set_name!r}; error={app.error_text()!r}"
    )


def open_bound_row(app, set_name: str, *, diagnose=None) -> str:
    """Leave ``set_name``'s row open with its bound location's switch painted,
    and return the row's scope.

    The row's index is re-read every time: a second set can sort ahead of the
    first and move it. Checked on the row's OWN switch, not an unscoped body
    widget, since another set's row may be open too; re-expanded on any poll
    that finds the row shut, because linux rebuilds the list (collapsing every
    row) on each observer tick (`BackupActions.find_and_expand_folder_until`'s
    reason), and the windows list may rebuild after a bind. Shut is read off the
    binding form's path input, which every open owner row paints
    (`BackupActions.open_folder`'s probe): the expand is a toggle, so clicking a
    row that is open but still painting its bound locations would shut it.

    ``diagnose`` returns the failure's account of the engine (default: the app's
    own agent tail, :func:`agent_diagnosis`); a seat run passes its seat's.
    """
    b = app.backups
    b.navigate_folders()
    deadline = time.monotonic() + 60
    while True:
        scope = f"folder-row[{b.wait_for_folder_row(set_name)}]"
        if app.sync_locations.mode_toggle_visible(scope=scope):
            return scope
        if time.monotonic() >= deadline:
            engine = diagnose() if diagnose is not None else agent_diagnosis(app, "owner")
            pytest.fail(
                f"{set_name!r}'s row never showed its folder-location-mode-toggle; "
                f"{app.driver.diagnose('folder-location-mode-toggle')} "
                f"{app.driver.diagnose('folder-location-path-input')} "
                f"{app.driver.diagnose('folder-location-path')} "
                f"error={app.error_text()!r}\n{engine}"
            )
        if not app.driver.is_visible_scrolled("folder-location-path-input", scope=scope):
            b.expand_folder(b.wait_for_folder_row(set_name))


def _log_path(relative: str) -> str:
    """The redacted form a sync-engine log line carries for ``relative`` —
    the Python twin of ``fauna_core::log_redact::log_path``: ``path~`` + the
    first 6 bytes (12 hex chars) of the plain BLAKE3 ``path_hash`` over the
    forward-slash relative path."""
    import blake3

    normalized = relative.replace("\\", "/")
    return "path~" + blake3.blake3(normalized.encode()).hexdigest()[:12]


def log_folder_name(name: str) -> str:
    """The redacted form a sync-engine log line carries for the set ``name`` —
    the Python twin of ``fauna_core::log_redact::log_folder_name`` for a
    user-chosen name: ``name~`` + the first 6 bytes (12 hex chars) of its
    ``set_name_hash``."""
    from helpers.set_names import set_name_hash

    return "name~" + set_name_hash(name).hex()[:12]


def await_agent_log(
    app, needle: str, *, seat: str, also: str = "", window: float = SYNC_WINDOW_SECS
) -> None:
    """Poll the agent's own log until one line carries ``needle`` (and ``also``).

    The same witness ``await_agent_upload`` reads — the real ``fauna-sync-agent``
    on the app's stderr fd — for a verdict the engine reaches without recording
    anything a nest read could show. A deadline poll on state the process itself
    reports, never a settle-sleep (convention 14).
    """
    deadline = time.monotonic() + window
    while time.monotonic() < deadline:
        try:
            text = app.driver.app_stderr_text()
        except Exception:
            text = ""
        for line in text.splitlines():
            if needle in line and also in line:
                return
        time.sleep(2.0)
    pytest.fail(
        f"[{seat}] the agent never logged {needle!r}"
        + (f" for {also!r}" if also else "")
        + f" within {window:.0f}s. error={app.error_text()!r}\n"
        + agent_diagnosis(app, seat)
    )


def upload_count(app, filename: str) -> int:
    """How many times the agent's engine has reported uploading ``filename`` —
    the same ``file uploaded`` witness :func:`await_agent_upload` reads, counted,
    for a caller that must tell a file's SECOND upload (a change) from its first.
    """
    logged = _log_path(filename)
    try:
        text = app.driver.app_stderr_text()
    except Exception:
        return 0
    return sum(1 for line in text.splitlines() if "file uploaded" in line and logged in line)


def hold_custody_reread(app) -> Path:
    """Hold this seat's agent from re-reading custody: create the file its
    launch's ``FAUNA_E2E_CUSTODY_HOLD_FILE`` names (the compile-gated seam
    ``content_keys::custody_reread_held``). While it exists each resolve keeps
    the custody reading the agent holds and takes only the fresh folder list,
    so a rotation that raised a set's floor reads *Keys pending* until
    :func:`release_custody_reread`. Returns the file. Loud, never a skip, on a
    driver that sets no hold file: the test would otherwise pass vacuously."""
    path = getattr(app.driver, "custody_hold_file", lambda: None)()
    if not path:
        raise RuntimeError(
            f"{type(app.driver).__name__} sets no FAUNA_E2E_CUSTODY_HOLD_FILE "
            "(the tui, linux, macos and windows drivers do, at launch)"
        )
    hold = Path(path)
    hold.parent.mkdir(parents=True, exist_ok=True)
    hold.touch()
    return hold


def release_custody_reread(app) -> None:
    """Release :func:`hold_custody_reread`: delete the hold file. The agent
    re-reads custody at its next edge — the resident tick's row read, which
    wakes the custody task while a bound set's floor is ahead of the generation
    it holds."""
    path = getattr(app.driver, "custody_hold_file", lambda: None)()
    if path:
        Path(path).unlink(missing_ok=True)


def custody_hold_applied(app) -> bool:
    """Whether this seat's agent has kept a held custody reading at least once —
    its ``custody re-read held by the e2e custody hold`` log line. The proof a
    hold took effect, for a failure message that must tell "the hold was never
    adopted" (a release build, an unset variable) from "the status never read
    it"."""
    try:
        text = app.driver.app_stderr_text()
    except Exception:
        return False
    return "custody re-read held by the e2e custody hold" in text


def engine_restart_count(app, set_name: str) -> int:
    """How many times this seat's agent has restarted ``set_name``'s engine —
    ``engine_driver.rs``'s ``"…; restarting engine"`` line, which names the set
    REDACTED (:func:`log_folder_name`), so matching the plaintext name never
    fires. **Counted, never merely matched:** binding the set already produces
    one restart (unbound → keyed), so a barrier waits for the count to rise past
    a baseline taken after the bind."""
    logged = log_folder_name(set_name)
    try:
        text = app.driver.app_stderr_text()
    except Exception:
        return 0
    return sum(
        1 for line in text.splitlines() if "restarting engine" in line and logged in line
    )


def await_agent_upload(
    app, filename: str, *, seat: str, window: float = SYNC_WINDOW_SECS
) -> None:
    """Poll the agent's own log until it reports uploading ``filename``.

    The witness is the production process's own output: the real
    ``fauna-sync-agent`` runs as a child of the app on the same stderr fd, and
    ``fauna_sync_engine::engine`` emits ``file uploaded path="…"`` when the byte
    plane has accepted a file. That is a genuine mechanism observable — the
    engine cannot log it without having uploaded.

    Deliberately NOT the Media listing: the Media page refreshes only when it
    *becomes visible* and never re-polls (``MediaMachine::refresh`` fires on the
    nav edge; ``set_filter`` is pure render state), so a listing-based wait is a
    wait on the *reader's* nav choreography, not on the upload. A caller that
    wants the listing must re-enter the page after this returns — see
    ``tests/test_folder_member_media_decrypt.py``.
    """
    needle = "file uploaded"
    # The engine logs the path REDACTED — `fauna_core::log_redact::log_path`,
    # `path~` + the first 6 bytes of its BLAKE3 `path_hash` in hex (the log
    # scrub, `file-sync.md` § Sealed names & paths) — so matching the plaintext
    # name never fires. Callers pass the bound-root-relative path the engine
    # records.
    logged = _log_path(filename)
    deadline = time.monotonic() + window
    while time.monotonic() < deadline:
        try:
            text = app.driver.app_stderr_text()
        except Exception:
            text = ""
        for line in text.splitlines():
            if needle in line and logged in line:
                return
        time.sleep(2.0)
    pytest.fail(
        f"[{seat}] the agent never reported uploading {filename!r} within "
        f"{window:.0f}s — the app→agent→engine→nest upload path did "
        f"not complete. error={app.error_text()!r}\n" + agent_diagnosis(app, seat)
    )

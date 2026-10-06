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
    ``/tmp`` (on-demand-files.md § Linux FUSE binding, the lifecycle rule). The
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
    try:
        table = Path("/proc/self/mountinfo").read_text()
    except OSError:
        return False
    target = str(location)
    for line in table.splitlines():
        left, _, right = line.partition(" - ")
        fields = left.split()
        # mountinfo escapes a space in the mount point as `\040`.
        if len(fields) > 4 and fields[4].replace("\\040", " ") == target:
            if right.split()[:1] == ["fuse.fauna"]:
                return True
    return False


def bind_location_under_set(app, set_name: str, location: Path, *, seat: str) -> int:
    """Bind ``location`` under ``set_name``'s expander and wait for the row.

    The binding form is nested per set (the set is contextual — there is no
    free-text set name), so the unindexed ``folder-location-*`` IDs resolve to the
    one expanded set.
    """
    b = app.backups
    b.navigate_folders()
    idx = b.find_and_expand_folder(set_name)
    # `expand_folder` TOGGLES the ExpanderRow, so calling it on a row this test
    # already expanded (the owner's, from the share gesture) closes it and the
    # nested binding form disappears. Re-open when that happened, so this helper
    # works from either prior state.
    if not app.driver.is_visible("folder-location-path-input"):
        b.expand_folder(idx)
    app.driver.type_text("folder-location-path-input", str(location))
    app.driver.click("folder-location-add-button")

    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
            return idx
        time.sleep(0.3)
    pytest.fail(
        f"[{seat}] bound location never rendered as a folder-location-row under "
        f"{set_name!r}; error={app.error_text()!r}"
    )


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

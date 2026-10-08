"""File-shape scenarios between app seats — the shared-contract half of the
retired headless-daemon sync suites, ported onto signing seats.

Why this exists
---------------
Until 2026-09-30 the file-shape cases of device-to-device sync — a shrinking
overwrite, delete-then-recreate, a batch, empty / NUL / CRLF / binary bodies,
non-ASCII names, nested directories, a rename, rapid rewrites, a multi-chunk
file, a delete travelling the other way, an overlapping concurrent edit — were
proven by pairs of headless ``fauna-sync`` daemons
(``tests/platform/sync/test_file_sync.py``, ``test_file_shrink.py``,
``test_sync_large_file.py``, ``test_bug_repros.py``, ``test_text_merge.py``
and the windows-only ``tests/platform/windows/test_sync_*.py``). The
nest refuses every unsigned record, which is every record the daemon's data
plane sends (``mls-group-key-material.md`` § Implementation status
today, the legacy-daemon residual), so none of them could record a byte.

The cases themselves are engine behaviour, not daemon behaviour — the same
``fauna-sync-engine`` runs inside every app's ``fauna-sync-agent`` — so they
are kept, as one plan run against the app seats of ``helpers/sync_seats.py``
(``tests/test_filesync_seats.py::test_seat_scenarios``). One plan on every
platform replaces a cross-platform suite plus a windows-only copy of it
(priority #1).

The observation discipline (convention 14)
------------------------------------------
Every positive expectation is a deadline poll on exact BYTES — never text, so
NUL and CRLF bodies compare honestly. Every absence is asserted only after the
same path was proven present on that seat by an earlier await in the plan, so
"absent" is always a witnessed present→absent transition, never a file that
simply never arrived.

Pure data and pure polling: no driver, no process. The seats are whatever
``SeatDriver`` objects the caller hands in.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

# Larger than the chunker's single-chunk threshold (8 MiB,
# `fauna_core::chunker::MAX_CHUNK`), so FastCDC cuts it into several chunks —
# the F4 multi-chunk shape the daemon suite proved with 65 MiB. Three maximum
# chunks' worth is enough to need reassembly; random bytes keep compression
# from shrinking it below the threshold.
LARGE_FILE_BYTES = 24 * 1024 * 1024

BATCH_SIZE = 20


@dataclass
class Step:
    """One act on one seat, then what every OTHER seat must come to hold.

    ``expect`` maps a folder-relative path to the exact bytes each peer must
    hold. ``gone`` lists folder-relative paths each peer must no longer hold —
    each one must have been expected present by an earlier step, which is what
    makes the absence a witnessed transition. ``converge`` lists paths that must
    end up byte-identical on EVERY seat, the writer included, with a body from
    ``converge_bodies`` — the shape of a concurrent edit, where which side wins
    is the engine's call and not the test's.
    """

    name: str
    writer: int
    act: Callable[[Path], None]
    expect: dict[str, bytes] = field(default_factory=dict)
    gone: list[str] = field(default_factory=list)
    converge: list[str] = field(default_factory=list)
    converge_bodies: tuple[bytes, ...] = ()
    # A second act, run on a DIFFERENT seat right after `act` and before any
    # await — the other half of a concurrent edit.
    other_writer: int | None = None
    other_act: Callable[[Path], None] | None = None


def _write(path: Path, body: bytes) -> None:
    """Write via temp+rename, so a watcher never observes a half-written body."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.tmp")
    tmp.write_bytes(body)
    os.replace(tmp, path)


def _read(path: Path) -> bytes | None:
    try:
        return path.read_bytes()
    except OSError:
        return None


def _digest(body: bytes | None) -> str:
    if body is None:
        return "absent"
    return f"{len(body)} B sha256:{hashlib.sha256(body).hexdigest()[:16]}"


def build_plan(token: str) -> list[Step]:
    """The scenario plan for one run, every path prefixed with ``token`` so the
    live box's cleanup and residue sweep (``sync_seats.finalize_live_residue``)
    recognise it."""
    t = token
    batch = {f"{t}-batch-{i:02d}.txt": f"batch file {i}\n".encode() for i in range(BATCH_SIZE)}
    large = os.urandom(LARGE_FILE_BYTES)
    special = {
        f"{t}-empty.bin": b"",
        f"{t}-nul.bin": b"head\x00middle\x00\x00tail",
        f"{t}-crlf.txt": b"line one\r\nline two\r\n\r\nline four\r\n",
        f"{t}-binary.bin": bytes(range(256)) * 16,
    }
    names = {
        f"{t}-caf\u00e9-\u00e0cc\u00e9nt\u00e9d.txt": "accented name\n".encode(),
        f"{t}-\u65e5\u672c\u8a9e-\u6587\u66f8.txt": "cjk name\n".encode(),
    }
    nested = f"{t}-nested/deeper/deepest/file.txt"
    clash_a = b"clash: seat a wrote this line\n"
    clash_b = b"clash: seat b wrote this other line\n"
    clash_bin_a = b"\x00\x01seat-a-binary\xff"
    clash_bin_b = b"\x00\x02seat-b-binary\xfe\xfd"

    def write_all(files: dict[str, bytes]) -> Callable[[Path], None]:
        def act(root: Path) -> None:
            for rel, body in files.items():
                _write(root / rel, body)
        return act

    def write_one(rel: str, body: bytes) -> Callable[[Path], None]:
        return write_all({rel: body})

    def unlink(rel: str) -> Callable[[Path], None]:
        return lambda root: (root / rel).unlink()

    def rename(old: str, new: str) -> Callable[[Path], None]:
        return lambda root: os.replace(root / old, root / new)

    def rapid(rel: str, bodies: list[bytes]) -> Callable[[Path], None]:
        def act(root: Path) -> None:
            for body in bodies:
                _write(root / rel, body)
        return act

    edit = f"{t}-edit.txt"
    recreate = f"{t}-recreate.txt"
    old, new = f"{t}-old-name.txt", f"{t}-new-name.txt"
    rapid_path = f"{t}-rapid.txt"
    clash, clash_bin = f"{t}-clash.txt", f"{t}-clash.bin"
    large_path = f"{t}-large.bin"
    return [
        # Grow, then SHRINK: the overwrite with a shorter body must not leave
        # the old tail behind (the F2 shrink bug, test_file_shrink.py).
        Step("create", 0, write_one(edit, b"short\n"), expect={edit: b"short\n"}),
        Step("grow", 0, write_one(edit, b"a much longer body than before\n" * 8),
             expect={edit: b"a much longer body than before\n" * 8}),
        Step("shrink", 0, write_one(edit, b"x\n"), expect={edit: b"x\n"}),
        # Delete, then recreate under the same name with new content.
        Step("recreate-1", 0, write_one(recreate, b"first life\n"),
             expect={recreate: b"first life\n"}),
        Step("recreate-delete", 0, unlink(recreate), gone=[recreate]),
        Step("recreate-2", 0, write_one(recreate, b"second life\n"),
             expect={recreate: b"second life\n"}),
        Step("batch", 0, write_all(batch), expect=dict(batch)),
        Step("special-bytes", 0, write_all(special), expect=dict(special)),
        Step("non-ascii-names", 0, write_all(names), expect=dict(names)),
        Step("nested-dirs", 0, write_one(nested, b"three levels down\n"),
             expect={nested: b"three levels down\n"}),
        # A rename is a delete of the old name plus a create of the new one.
        Step("rename-create", 0, write_one(old, b"renamed body\n"),
             expect={old: b"renamed body\n"}),
        Step("rename", 0, rename(old, new), expect={new: b"renamed body\n"}, gone=[old]),
        # Rapid rewrites: the last body wins, never an intermediate one.
        Step("rapid-rewrites", 0,
             rapid(rapid_path, [f"rapid v{i}\n".encode() for i in range(1, 6)]),
             expect={rapid_path: b"rapid v5\n"}),
        Step("large-multi-chunk", 0, write_one(large_path, large),
             expect={large_path: large}),
        # The other direction: seat b deletes a file seat a wrote.
        Step("reverse-delete", 1, unlink(edit), gone=[edit]),
        # Overlapping concurrent edits of one line (text) and of a binary: no
        # three-way merge exists, so the engine's latest-wins fallback picks one
        # body and every seat must converge on the SAME one
        # (test_text_merge.py::test_latest_wins_fallback_overlapping_and_binary).
        Step("clash-base", 0,
             write_all({clash: b"clash: base\n", clash_bin: b"\x00base"}),
             expect={clash: b"clash: base\n", clash_bin: b"\x00base"}),
        Step("clash", 0, write_all({clash: clash_a, clash_bin: clash_bin_a}),
             other_writer=1, other_act=write_all({clash: clash_b, clash_bin: clash_bin_b}),
             converge=[clash, clash_bin],
             converge_bodies=(clash_a, clash_b, clash_bin_a, clash_bin_b)),
    ]


def step_count() -> int:
    """How many awaited steps a run has — what a cell's timeout ceiling derives
    from. The plan's shape does not depend on the token."""
    return len(build_plan("0seat-00000000-00000000")) + 1  # + the cleanup step


def _fail(message: str, note: Callable[[], str]) -> None:
    raise AssertionError(f"{message}\n{note()}")


def _await(seat, want: Callable[[], list[str]], phase: str, budget: float, note) -> None:
    """Deadline-poll until ``want()`` reports nothing outstanding on ``seat``."""
    deadline = time.monotonic() + budget
    outstanding = want()
    while outstanding and time.monotonic() < deadline:
        time.sleep(1.0)
        outstanding = want()
    if outstanding:
        _fail(
            f"[{seat.name}] {phase}: not converged within {budget:.0f}s:\n  "
            + "\n  ".join(outstanding)
            + f"\n  self-check: {seat.self_note()}",
            note,
        )


def _act_on(seat, act: Callable[[Path], None], step_name: str, note) -> None:
    """Run ``act`` in ``seat``'s folder; a refusal from the folder itself fails
    as that seat's, with every seat's diagnostics (convention 6).

    The writer's own folder can refuse plain file I/O — on windows a cfapi root
    whose provider never answers the create's placeholder fetch surfaces as a
    bare ``OSError`` EINVAL — and the agent log saying why is in the note, not
    in the error."""
    try:
        act(seat.path)
    except OSError as e:
        _fail(
            f"[{seat.name}] {step_name}: the act failed on this seat: {e!r}\n"
            f"  self-check: {seat.self_note()}",
            note,
        )


def run_step(step: Step, seats, budget: float, note) -> None:
    """Act, then await every expectation on every peer (and, for ``converge``,
    on every seat)."""
    writer = seats[step.writer]
    peers = [s for i, s in enumerate(seats) if i != step.writer]
    _act_on(writer, step.act, step.name, note)
    if step.other_act is not None:
        _act_on(seats[step.other_writer], step.other_act, step.name, note)
    print(f"[scenarios] {step.name}: seat {writer.name} acted, awaiting peers", flush=True)

    for peer in peers:
        def missing(peer=peer) -> list[str]:
            out = []
            for rel, body in step.expect.items():
                got = _read(peer.path / rel)
                if got != body:
                    out.append(f"{rel}: want {_digest(body)}, have {_digest(got)}")
            for rel in step.gone:
                if (peer.path / rel).exists():
                    out.append(f"{rel}: still present (the delete never applied here)")
            return out

        _await(peer, missing, step.name, budget, note)

    if step.converge:
        def disagree() -> list[str]:
            out = []
            for rel in step.converge:
                bodies = {s.name: _read(s.path / rel) for s in seats}
                values = set(bodies.values())
                if len(values) != 1 or next(iter(values)) not in step.converge_bodies:
                    out.append(
                        f"{rel}: "
                        + ", ".join(f"{n}={_digest(b)}" for n, b in bodies.items())
                    )
            return out

        _await(seats[0], disagree, f"{step.name} (all seats agree)", budget, note)


def run_scenarios(seats, run_token: str, *, window: float) -> None:
    """The whole plan against ``seats`` (two or more), then an assertive cleanup."""
    seats = list(seats)
    assert len(seats) >= 2, f"scenarios need a writer and a peer; got {len(seats)} seat(s)"

    def note() -> str:
        return "\n".join(s.diagnostics() for s in seats)

    for step in build_plan(run_token):
        run_step(step, seats, window, note)

    # Cleanup: every file this run created, deleted on seat a and witnessed
    # leaving every peer — each was proven present by the plan above, so an
    # absence here is a real applied delete. Directories are then removed on
    # every seat locally (a directory is not a synced object).
    deleter = seats[0]
    created = []
    for top in deleter.path.glob(f"{run_token}-*"):
        files = [top] if top.is_file() else [p for p in top.rglob("*") if p.is_file()]
        created += [str(p.relative_to(deleter.path)).replace(os.sep, "/") for p in files]
    created = sorted(created)
    assert created, (
        f"[{deleter.name}] nothing matching {run_token}-* is in this seat's folder "
        f"at cleanup time, so the run cannot verify it removed what it created.\n{note()}"
    )
    for rel in created:
        (deleter.path / rel).unlink()
    run_step(Step("cleanup", 0, lambda _root: None, gone=created), seats, window, note)
    for seat in seats:
        for p in seat.path.glob(f"{run_token}-*"):
            if p.is_dir():
                shutil.rmtree(p, ignore_errors=True)
    print(f"[scenarios] {len(created)} files converged and cleaned up — PASS", flush=True)


__all__ = [
    "BATCH_SIZE",
    "LARGE_FILE_BYTES",
    "Step",
    "build_plan",
    "run_scenarios",
    "run_step",
    "step_count",
]

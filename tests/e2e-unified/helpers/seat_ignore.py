"""The ignore scenario between app seats — what a writer's engine must NEVER
take up, proven with a causal barrier rather than a settle-sleep.

Why this exists
---------------
The daemon's rescan-ignore e2e (``test_faunaignore``, ``test_rescan_no_resend``)
retired with the daemon as a writer, and nothing replaced it on the agent the
apps run. The rules themselves are owned by
``docs/goal/behavior/sync-engine-deployments.md`` § Built-in default ignores:
the scanner excludes dotfiles categorically, ``.faunaignore`` adds per-folder
gitignore-style patterns, and ``DEFAULT_IGNORE_PATTERNS`` (``~$*``, ``*.tmp``,
``Thumbs.db``, ``desktop.ini``) is unioned with it unconditionally.

Three legs, one barrier
-----------------------
The writer's folder is SEEDED before its seat starts — ``.faunaignore`` is read
once, when the engine is built (``SyncEngine``'s ``ignore`` field doc), so it has
to be on disk first — with one file per rule plus a kept control:

* a ``.faunaignore`` pattern (:data:`FAUNAIGNORE_PATTERN`),
* a built-in default (``*.tmp``),
* a dotfile (the scanner's categorical rule, ``ignore::has_hidden_component``),
* a kept file every peer must receive.

The barrier is the writer's first CLEAN local converge pass
(:meth:`sync_seats.AppSeatDriver.await_clean_converge_pass`): the engine was
built after the seed, so that pass scanned every seeded file and wrote a
``sync_entries`` row for each one it took up before logging the line. After it,
an ignored path with no row is the engine's decision; a row means the matcher
let it through (convention 14 — a causal anchor, never "the kept file arrived,
so the others never will"). The kept file reaching the peer is the positive
control that the folder really syncs, not the barrier.

Pure data and pure polling, like ``seat_scenarios``: no driver, no process.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

# A pattern no built-in default covers, so this leg fails only when the
# `.faunaignore` half of the matcher is not applied.
FAUNAIGNORE_PATTERN = "*.secret"


@dataclass(frozen=True)
class IgnorePlan:
    """The seeded files, by the rule that must keep each one local."""

    kept: str
    ignored: dict[str, str]  # rel -> the rule that must exclude it

    def bodies(self) -> dict[str, bytes]:
        rels = [self.kept, *self.ignored]
        return {rel: f"{rel} body\n".encode() for rel in rels}


def build_plan(token: str) -> IgnorePlan:
    return IgnorePlan(
        kept=f"{token}-kept.txt",
        ignored={
            f"{token}-notes.secret": f".faunaignore pattern {FAUNAIGNORE_PATTERN!r}",
            f"{token}-scratch.tmp": "built-in default '*.tmp'",
            f".{token}-hidden.txt": "the scanner's dotfile rule",
        },
    )


def seed(watch: Path, plan: IgnorePlan) -> None:
    """Write ``.faunaignore`` and every planned file into ``watch`` — before the
    seat that binds it starts."""
    watch.mkdir(parents=True, exist_ok=True)
    (watch / ".faunaignore").write_text(f"{FAUNAIGNORE_PATTERN}\n", encoding="utf-8")
    for rel, body in plan.bodies().items():
        (watch / rel).write_bytes(body)


def leaked(plan: IgnorePlan, entry_paths: set[str]) -> list[str]:
    """The ignored rels the writer's engine took up anyway, each with its rule."""
    return [f"{rel} (must be kept local by {rule})"
            for rel, rule in plan.ignored.items() if rel in entry_paths]


def run_ignore_scenario(seats, run_token: str, *, window: float) -> None:
    writer, peers = seats[0], seats[1:]
    plan = build_plan(run_token)

    def note() -> str:
        return "\n".join(s.diagnostics() for s in seats)

    waited = writer.await_clean_converge_pass(window)
    print(f"[ignore] {writer.name}: clean converge pass (+{waited:.0f}s)", flush=True)

    entries = writer.sync_entry_paths()
    # Positive control: the pass did scan the seeded folder — else an empty
    # table would pass every absence below.
    assert plan.kept in entries, (
        f"[{writer.name}] the clean pass left no sync_entries row for the kept "
        f"file {plan.kept!r}, so it never scanned the seeded folder and the "
        f"absences below would prove nothing. rows: {sorted(entries)}\n{note()}"
    )
    bad = leaked(plan, entries)
    assert not bad, (
        f"[{writer.name}] the engine took up files its ignore rules must keep "
        f"local:\n  " + "\n  ".join(bad) + f"\n{note()}"
    )

    want = plan.bodies()[plan.kept]
    for peer in peers:
        _await_bytes(peer, plan.kept, want, window, note)
        # By implication of the writer's rows (nothing ignored was ever handed
        # to the uploader); checked anyway, as the user-visible half.
        present = [rel for rel in plan.ignored if (peer.path / rel).exists()]
        assert not present, (
            f"[{peer.name}] holds ignored files from {writer.name}: {present}\n{note()}"
        )


def _await_bytes(seat, rel: str, want: bytes, budget: float, note: Callable[[], str]) -> None:
    deadline = time.monotonic() + budget
    path = seat.path / rel
    while time.monotonic() < deadline:
        try:
            if path.read_bytes() == want:
                return
        except OSError:
            pass
        time.sleep(1.0)
    raise AssertionError(
        f"[{seat.name}] the kept file {rel!r} never arrived within {budget:.0f}s "
        f"— the folder does not sync at all, so the ignore legs are moot.\n"
        f"  self-check: {seat.self_note()}\n{note()}"
    )

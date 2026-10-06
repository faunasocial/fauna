"""tier_4 (live-remote, OPT-IN, THREE-MACHINE): cross-machine filesync convergence
against a live nest (example.com), one seat per dev machine, all started manually
at roughly the same time.

EVERY invocation below needs ``FAUNA_E2E_LIVE=1`` exported — the repo-wide opt-in
for anything that drives the shared live box (pytest.ini § ``live_box``, enforced
by conftest's autouse ``_live_box_opt_in_gate``). Without it every test here
skips with that instruction. This module used to gate on ``~/.fauna-id`` merely
EXISTING, which is true on every dev machine, so a plain
``pytest tests/e2e-unified/tests/ --app tui`` sweep signed in to the live box and
ran the seat steps unattended; those failures were then filed twice as
regressions. The seed is still required —
it just no longer doubles as the opt-in.

Two tests form the coordinated handshake — the operator runs the reader on each
machine during `next`, confirms all three print the same id, then runs the seat
on `go`. A THIRD, ``test_preflight_this_seat_sees_the_shared_set``, stands alone:
one seat, no cohort, no writes, run with the SAME ``--client`` the seat will use,
to answer "can this machine's seat client even see the shared set's row?" before
three machines are committed to a round. Both failed rounds this module records
(``20260730-01`` and ``-02``) died on exactly that question, one step apart:

  * ``test_announce_next_run_id`` — the READER step, run during `next`. FIRST
    builds this machine's seat client (``_ensure_local_seat_built`` — the printed
    id is the cue to `go` everywhere at once, so announcing must mean "this seat
    is ready to run", never "ready in ten minutes"), then signs a client into the
    live account and reads the ``e2e-multiseat`` set THROUGH the
    Media explorer UI to compute the next run id ``YYYYMMDD-NN`` (see "Run id"
    below), printing ``Ready to start RUN_ID=...``. The reader client is a
    PARAMETER — the Media UI is identical on every app, so any client works;
    it defaults to the terminal client (fast, cross-platform) on all three
    machines now that the Windows terminal-client e2e driver has landed
    (2026-07-18 — ConPTY via pywinpty, ``drivers/conpty.py``; ``--client windows``
    still works as a fallback). Run it by node id:

        export FAUNA_E2E_LIVE=1
        pytest tests/test_filesync_multiseat_live.py::test_announce_next_run_id \\
            --client tui -s        # tui on all three machines

  * ``test_three_seat_live_sync`` — the SEAT run, run on `go` with the announced
    id exported, one per machine:

        export FAUNA_E2E_LIVE=1
        export FAUNA_MULTISEAT_RUN_ID=<the announced YYYYMMDD-NN>
        pytest tests/test_filesync_multiseat_live.py::test_three_seat_live_sync \\
            --client linux    # macOS -> --client macos, Windows -> --client windows

    ONE-INVOCATION MODE (2026-07-29; preferred on a loaded box). Passing both node
    ids to a single pytest run makes the announce hand its computed id straight to
    the seat (``_publish_run_id``), so the round pays the machine-wide ``e2e``
    slot queue ONCE instead of twice — conftest takes that slot at
    ``pytest_collection_finish`` and holds it for the whole session:

        pytest tests/test_filesync_multiseat_live.py::test_announce_next_run_id \\
               tests/test_filesync_multiseat_live.py::test_three_seat_live_sync \\
            --client tui -s     # Linux/macOS; on Windows use --client windows for both

    Why this matters: the 2026-07-29 FIFO ticket queue ended indefinite slot
    starvation but bounds a waiter's POSITION, not its DURATION (an ``--app``
    sweep holds a slot 15+ min), so two acquisitions still cost two real waits
    with operator latency between them. The slot must never be held ACROSS the
    operator gate — that idle-holds 1 of 2 machine-wide slots for human latency
    and starves concurrent sessions exactly as this round itself was starved on
    2026-07-29. Collapsing the two
    invocations is how the queue is paid once without ever holding a slot for a
    human. The trade: there is no mid-run pause for the operator to compare the
    printed ids, so the cohort's skew must be absorbed by the test — which is
    what ``ASSEMBLY_WINDOW`` is for. The id is a pure function of the set, so
    every machine computes the same one independently; a genuine divergence
    (a midnight rollover) surfaces as a loud phase-1 failure naming what each
    seat could see, never as silent corruption. The env-var override keeps
    precedence, so the two-invocation operator-compare flow above is unchanged.

    The SEAT is the MACHINE (the Linux machine plays ``linux``, the macOS machine
    ``macos``, the Windows machine ``windows`` — the seat name prefixes this
    seat's run files and must match the cohort); the CLIENT is a parameter for how
    that seat is driven. The terminal client can drive it instead
    (``--client tui``): it wires the same shared-Rust agent provisioner as linux,
    and `just tui-debug` builds `fauna-sync-agent` as its sibling — the layout the
    agent's own sibling probe resolves. That makes a tui seat the cheap way to
    separate "this machine's sync is broken" from "this machine's GUI app is
    broken", and on a windows seat it also skips the desktop app's long MSBuild.
    ⚠ This comment used to assert the terminal client was unix-only
    (`apps/fauna-tui/src/sync_agent.rs` `#[cfg(unix)]`, `not(unix)` spawning no
    agent). That is STALE: the provisioner is `#[cfg(any(unix, windows))]` with a
    detached spawner, so every seat platform can be driven this way. Only a
    native `--client` makes a round a desktop-app proof — that, not availability,
    is the reason to choose one. When the
    seat will be driven by a non-native app, tell the ANNOUNCE so it builds the
    right one: ``FAUNA_MULTISEAT_SEAT_CLIENT=tui`` (the announce runs in a separate
    process and cannot see the seat run's ``--client``).

    THE CLIENT-FREE SEAT (2026-07-31 — RETIRED 2026-09-30). A third seat
    flavour once drove no client at all: a headless ``fauna-sync`` daemon
    running this module's run-file protocol. The nest refuses every change record
    without a writer signature and the daemon's data plane sends none, so that
    seat could not write, and it was removed. The cheapest bisect for a client
    defect is now the one-machine ``tui+tui`` cell in
    ``test_filesync_seats.py``.

Each seat signs the native desktop app into the SAME live account (three
devices of one user), binds a fresh local folder to the shared sync-type file
set ``e2e-multiseat``, and then coordinates with the other two seats purely
THROUGH THE SYNC ENGINE — there is no side channel between the machines, so
every rendezvous barrier is itself an assertion that sync works. Every run
file's basename carries the full run id as its prefix (``<ID>-hello-linux.txt``,
``<ID>-shared.txt``, …), so the run is self-marking — the announce reads those
names back through the Media UI to pick the next id, and no separate marker file
is needed:

  phase 0  freshness self-check: before writing anything, each seat reads the set
           through the Media explorer and asserts its OWN ``<ID>-*-<seat>`` files
           are absent — a reused id (a stale manual override, a duplicated `go`)
           is caught here, loudly, instead of corrupting a rendezvous. This also
           exercises the Media UI on all three GUI seats.
  phase 1  each seat writes ``<ID>-hello-<seat>.txt``; the CREATOR seat (the
           canonical-first seat present in the cohort) also creates
           ``<ID>-shared.txt`` at a known base (single writer — no create/create
           conflict on the merge base). Every seat waits until it observes the
           other cohort seats' hello files AND the shared base, byte-exact. This
           is the "each app creates one file, the others observe it" scenario,
           and passing it proves every participating engine is live, mutually
           syncing, and holds the same merge base. This is the cohort's ASSEMBLY
           point and the only ASYMMETRIC wait — a peer's phase 1 is a pure wait on
           the creator's output — so it alone is budgeted at ``ASSEMBLY_WINDOW``,
           generous enough to absorb another machine's e2e-slot queue and cold
           build. Phases 2-4 keep the tight ``WINDOW``: their point is that the
           edits genuinely overlap, which a late stroller would not prove.
  phase 2  each seat writes ``<ID>-ready-<seat>.txt`` and waits for the whole
           cohort, so the phase-3 edits genuinely overlap (within one sync latency).
  phase 3  each seat rewrites ONLY its own line of ``<ID>-shared.txt``. The three
           hunks are separated by unique spacer lines, so every pairwise
           three-way merge is clean by construction and merges commute
           (file-sync.md § Conflicts: text auto-merge when hunks don't overlap,
           never markers). Each seat polls its local shared file until it equals
           the fully-known expected merged content byte-for-byte — a latest-wins
           fallback would drop an edit and never reach it, and markers would
           break byte-equality, so exact convergence proves the merge path
           end-to-end.
  phase 4  each seat writes ``<ID>-done-<seat>.txt`` carrying the sha256 of the
           expected final content and waits for the other cohort seats' matching
           done files — proof that every participating machine independently
           observed the identical final state, not just this one. It then waits
           for its OWN ack to become visible on the nest
           (:func:`_await_own_ack_on_nest`): every other wait in this test is
           peer-only, so without that barrier the last seat to arrive passes
           while its ack is still undrained, stranding the peers that block on
           it (observed live, run 20260724-06).
  phase 5  cross-machine delete propagation, in two steps whose ORDER is the
           whole design. **5a** each seat writes ``<ID>-seen-<seat>.txt`` and
           waits for the cohort's: writing it means "my phase-4 wait returned",
           so observing a peer's is proof that peer already materialised MY done
           file. **5b** each seat then unlinks its OWN ``<ID>-done-<seat>.txt``
           and waits for both peers' to disappear locally. Three tombstones,
           each applied on two peers, cover every ordered (recording OS ->
           applying OS) pair in one run — the certification a single machine
           cannot give, and `file-sync.md` § *Applying a remote delete must not
           record one back* names why it must be cross-OS: the echo-suppression
           token collision is reachable on macOS's event latency and not on
           Linux's, so a green Linux run does not absolve a daemon of it.

Determinism despite skewed starts / speeds / network blips:
  - Every wait is poll-until-observed with a per-phase deadline (default 180 s,
    ``FAUNA_MULTISEAT_TIMEOUT_SECS``) counted from this seat's OWN previous
    step — seats started up to ~a window apart still rendezvous, and wall
    clocks are never consulted or compared.
  - The run id (``YYYYMMDD-NN``) prefixes every basename, so stale files from
    earlier/crashed runs can never satisfy a wait. It is not stored locally: the
    announce derives it from the set's own files (all three machines read the
    same nest-side listing and agree by construction), and it advances per
    attempt, so id-stamped files accumulate in the set as a few hundred bytes
    each -- clean them up manually whenever.
  - In-run cleanup is confined to phase 5, and ONLY the done files. Deleting a
    run's files from the fastest seat used to be unsafe at any point — the
    tombstone wins batch-latest, so a slower seat that had not yet materialised
    the file would never see it and would hang its phase-4 wait forever. Phase
    5a's barrier is exactly the missing precondition: a seat unlinks only after
    every peer has said it already holds the file. The other run files
    (hello / ready / shared / seen) are still left behind, because the last
    barrier's own files can have no such proof without another barrier after
    them — the regress has to stop somewhere, and stopping it at a few hundred
    bytes per run is the cheap end. Clean those up together (never partially),
    or a surviving stale file could satisfy a future wait.

What this test does NOT do: it never claims, never factory-resets (three
concurrent seats must all sign in — if the nest is unclaimed it fails fast and
tells you to claim it once first), and it calls no nest APIs — every mutation
is the client UI plus plain file I/O in the bound folder, exactly a user's
path (testing.md § point 8).

Zero-config (the operator sets NO environment variables -- helpers/
multiseat_config.py supplies every default; each is still env-overridable):
  seed       ~/.fauna-id (the account's ed25519 32-byte seed hex), else
             FAUNA_LIVE_SECRET_HEX. Absent everywhere -> the test skips.
  handle     FAUNA_LIVE_MAIL_ADDRESS, else a probe localpart on the nest URL's
             own domain (mail is not touched; the client discovers the nest by
             DoH from this handle's DOMAIN, and the nest replaces the localpart
             with the account's registered handle at sign-in — helpers/
             live_handle.py). It used to default to a hardcoded `test@example.com`,
             which was a guess about a value only the box knows.
  run id     the announce computes YYYYMMDD-NN by reading the set (above); the
             seat run takes it from FAUNA_MULTISEAT_RUN_ID (the session exports
             the announced value on `go`) and fails fast if it is unset.
  window     FAUNA_MULTISEAT_TIMEOUT_SECS, default 180 s per phase.
  cohort     FAUNA_MULTISEAT_SEATS, a comma-separated subset of
             linux,macos,windows (default all three) — e.g. `linux,macos` for a
             fast two-machine check. Every participating machine passes the SAME
             cohort; the rendezvous waits only for the cohort's seats.
  seat client
             FAUNA_MULTISEAT_SEAT_CLIENT, default the platform's native desktop
             app. Only the ANNOUNCE reads it (to build the right client); the
             seat run itself takes the client from `--client`.

Markers: tier_4 (live-remote — deployed production image under real supervision; ruling 2026-07-22) + live_box (opt-in, machine-local
flock vs. other live tests on the same box — cross-machine coordination is the
operator, i.e. you) + live_nest (every nest interaction targets the live box, so
a session of only these tests builds/starts NO local fauna-nest — the apps
launch pointed at the live URL and sign in via DoH; the cold ~15-25 min local
nest build every seat used to pay was pure waste) + real_sync_agent (the windows
seat needs the real per-user fauna-sync-agent.exe agent; harmless elsewhere) + tui (so
--client tui admits the announce reader). @pytest.mark.timeout(1200) bounds the
documented-long seat run.
"""

import hashlib
import os
import re
import sys
import time
from pathlib import Path

import pytest

from helpers import multiseat_config as cfg
from helpers.waiting import wait_until

# Zero-config: no operator ever sets an env var (helpers/multiseat_config.py owns
# every default). The seat run's id is computed by the announce reader and passed
# in via FAUNA_MULTISEAT_RUN_ID; it is empty during the announce (which computes
# its own) and required (fail-fast) in the seat run.
SECRET = cfg.load_secret()
ADDRESS = cfg.address()
RUN_ID = os.environ.get("FAUNA_MULTISEAT_RUN_ID", "")
WINDOW = cfg.window_secs()

# Phase 1 is the ONE wait that legitimately spans another MACHINE's queue. Every
# peer's phase 1 is a pure wait on the CREATOR's merge base, and since 2026-07-29
# the `e2e` slot pool grants in arrival order but says nothing about *duration*
# (`build-system.md` § Slot fairness: width 2 bounds overtaking at one position;
# an `--app` sweep still holds a slot 15+ min). So a peer that won its slot
# instantly used to burn its whole window while the creator was still queued, and
# failed a healthy cohort. Phases 2-4 keep the tight WINDOW on purpose — their
# point is that the edits genuinely overlap within one sync latency.
#
# DERIVED from WINDOW, never a bare constant, for the same reason the timeout
# below is: a deliberately-widened window is exactly what a loaded box needs, and
# a fixed ceiling would silently truncate it. A green run pays nothing — the
# deadline poll returns the instant the base is observed (convention 14).
ASSEMBLY_WINDOW = max(900.0, 2.0 * WINDOW)

# The one-seat preflight's budget for this client to render the shared set's row.
# NOT an assembly wait — no peer is involved and nothing is being created, so the
# only latency to absorb is this client's own sign-in → connect → folder list
# round trip. Deliberately short relative to WINDOW: the preflight's whole value
# is answering "can this seat see the set?" in a minute rather than in a
# 30-minute tri-machine round. A green run returns on the FIRST poll and pays
# none of it (convention 14) — the budget exists only to make the negative
# assertion sound, never to slow the positive one.
PREFLIGHT_WINDOW = 90.0


def _resolve_run_id(computed: str = "") -> str:
    """The run id, operator override winning over an in-process computation.

    `FAUNA_MULTISEAT_RUN_ID` keeps precedence so the documented two-invocation
    handshake (announce, operator compares the printed ids, `go`) behaves exactly
    as before. `computed` is what an in-session announce derived from the set.
    """
    return os.environ.get("FAUNA_MULTISEAT_RUN_ID", "").strip() or computed


def _publish_run_id(run_id: str) -> None:
    """Make `run_id` this pytest session's id, so ONE invocation can announce and
    then seat — and therefore pay ONE machine-wide `e2e`-slot acquisition.

    Why this matters more than it looks: conftest takes the `e2e` slot once at
    `pytest_collection_finish` and holds it for the whole session, so two
    invocations meant two trips through the queue with operator latency in
    between — and the slot must NOT be held across that gate (idle-holding 1 of 2
    machine-wide slots for human latency starves concurrent sessions exactly as
    this round itself was starved on 2026-07-29). Collapsing to one invocation is
    the way to pay the queue once without ever holding a slot for a human.

    Both sinks are load-bearing: the module global is what every run-file path
    helper reads, and the env var is what SPAWNED processes see (the app spawns
    the sync agent).
    """
    global RUN_ID
    RUN_ID = run_id
    os.environ["FAUNA_MULTISEAT_RUN_ID"] = run_id


# True once an ANNOUNCE has run in this pytest session. Load-bearing for one
# thing only: an announce-computed id is FRESH BY CONSTRUCTION —
# `next_run_id_from_names` returns 1 + the highest NN already present for today
# in the set's own nest-side listing, so no file with that id can exist yet. That
# makes the announce a second, independent freshness source for a seat whose own
# view of the set is unavailable (the client-free seat when the daemon's offline
# catch-up breaks — measured live 2026-07-31). It is deliberately NOT set by
# `_publish_run_id`, which a seat also calls: the claim is "a nest read computed
# this id", not "someone assigned it".
_ANNOUNCED_IN_SESSION = False


def _note_announced_in_session() -> None:
    global _ANNOUNCED_IN_SESSION
    _ANNOUNCED_IN_SESSION = True


def announced_in_session() -> bool:
    return _ANNOUNCED_IN_SESSION

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_box,
    pytest.mark.live_nest,
    # A three-machine round, invoked on its own with no `--nest`: every seat
    # dials the box `multiseat_config` names (example.com by default), never the
    # run's provider. A `--nest docker|live` sweep collecting it would aim a
    # round nobody announced at a box the run did not name — measured
    # 2026-10-05, a dev.example.com sweep sent all three at example.com.
    pytest.mark.standalone_only,
    pytest.mark.real_sync_agent,
    # BOTH real-agent markers, never just one (conftest `_apply_isolated_sync_agent_env`:
    # "a real-agent UI test needs BOTH markers"). `real_sync_agent` only gates whether
    # the hydration loop runs; `isolated_sync_agent` gives the app THIS run's own pipe,
    # agent binary and data dir, which it forwards to the agent IT spawns.
    #
    # The APP spawns the agent here — the test never does. That is deliberate and
    # uniform across all three desktop apps (linux's `ChildSpawner`, macOS's
    # `FfiChildAgentSpawner`, windows' `SpawnSyncAgentDetached`): a test-owned agent
    # would leave the app's spawn seam unexercised, and that seam silently doing
    # nothing is exactly what made the windows AND macOS seats look "bound" while
    # syncing in neither direction (2026-07-24). Testing the mechanism the product
    # actually uses is the point: a mechanism excused from testing is a mechanism
    # free to silently do nothing.
    pytest.mark.isolated_sync_agent,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
    pytest.mark.tui,
    # Documented-long live run (testing.md point 9: bounded always, unbounded
    # never). Six poll-until-observed rendezvous waits, each capped at its own
    # FAUNA_MULTISEAT_TIMEOUT_SECS counted from this seat's prior step; pytest.ini's
    # 900 s is too tight for the worst-case sum. This is a CEILING -- a healthy run,
    # with the client pre-built during `next`, converges in well under two minutes
    # once the cohort's seats are all up.
    #
    # DERIVED from WINDOW, never a bare constant: a raised window is exactly what a
    # loaded box or a slow-to-start peer needs (the build-slot queue alone cost 5 min
    # on the primary dev VM, 2026-07-24), and a fixed ceiling silently truncates it --
    # a 600 s window budgets 600 s PER WAIT, so the old flat 1200 hard-killed a
    # healthy-but-slow run mid-phase, which reads as a crash rather than a timeout.
    # +300 s covers the
    # client build/launch/sign-in prologue before phase 1. The 1200 floor keeps the
    # ratified default-window ceiling exactly as it was -- this only ever grows it.
    #
    # Phase 1 AND _adopt_or_create_set are each budgeted at ASSEMBLY_WINDOW rather
    # than WINDOW (see their comments), so the worst case is TWO sequential
    # assembly waits (a peer's folder-adoption wait, then its phase-1 rendezvous
    # wait -- both pure waits on the same CREATOR) plus the FIVE symmetric waits
    # (phases 2, 3, 4, 5a and 5b) plus the prologue. Counting either at the flat
    # WINDOW here would hard-kill a healthy-but-slow round mid-assembly, which
    # reads as a crash rather than a timeout -- the exact failure the derivation
    # exists to avoid. ⚠ The multiplier is not decoration: adding a wait without
    # growing it re-arms precisely that hard-kill, so phase 5 raised it 3 -> 5.
    pytest.mark.timeout(max(1200, int(2 * ASSEMBLY_WINDOW + 5 * WINDOW + 300))),
    pytest.mark.skipif(
        SECRET is None,
        reason="three-machine live filesync test: no account seed found. Put the "
        "32-byte ed25519 seed hex in ~/.fauna-id (or set FAUNA_LIVE_SECRET_HEX). "
        "Opt-in; started as `next` on the Linux+macOS+Windows sessions and run as "
        "the reader-then-seat handshake described in this module's docstring.",
    ),
]

_ALL_SEATS = ("linux", "macos", "windows")


def _cohort() -> tuple[str, ...]:
    """The participating seats, in canonical order. `FAUNA_MULTISEAT_SEATS` is a
    comma-separated subset (e.g. `linux,macos` for a fast two-machine check, when
    a third client is impractical to build); default is all three. The
    rendezvous adapts: each seat waits only for the cohort's OTHER seats, the
    shared file carries one line per cohort seat, and the canonical-first seat
    present (:data:`CREATOR`) is the single creator of the set + merge base. Every
    participating machine must pass the SAME cohort."""
    raw = os.environ.get("FAUNA_MULTISEAT_SEATS", "")
    if raw.strip():
        want = {s.strip().lower() for s in raw.split(",") if s.strip()}
        return tuple(s for s in _ALL_SEATS if s in want)
    return _ALL_SEATS


SEATS = _cohort()
CREATOR = SEATS[0] if SEATS else "linux"  # canonical-first present seat
SET_NAME = cfg.SET_NAME


# ── run-file paths (basename carries the id, so the run is self-marking) ──────
#
# The `run_id`/`seats` overrides make this module the ONE owner of the run-file
# protocol for every live sync test, not just the three-machine round: the
# one-machine seat tests (`tests/test_filesync_{two,three}seat.py`, testing.md
# § convention 16 — one file per seat count over the whole nest-mode axis since
# the live twins folded in) write the same bytes under their own
# `2seat-`/`3seat-` tokens and their own seat names. It cannot instead publish its token through
# `_publish_run_id`, which also writes `FAUNA_MULTISEAT_RUN_ID` into the
# environment and would hand a co-collected three-machine round an id no cohort
# ever agreed on.
#
# Both parameters are resolved at CALL time (never as a default expression —
# `RUN_ID` is rebound by `_publish_run_id`), and omitting them reproduces the
# three-machine bytes exactly, which `tests/test_sync_seats.py` pins. Only the
# helpers a two-seat run consumes are parametrized: `_ready`/`_done` belong to
# the cross-machine rendezvous and ack ceremony that convention 16 drops
# outright, so they keep reading the globals.


def _rid(run_id: str | None) -> str:
    return RUN_ID if run_id is None else run_id


def _hello_path(folder: Path, seat: str, run_id: str | None = None) -> Path:
    return folder / f"{_rid(run_id)}-hello-{seat}.txt"


def _ready_path(folder: Path, seat: str) -> Path:
    return folder / f"{RUN_ID}-ready-{seat}.txt"


def _done_path(folder: Path, seat: str) -> Path:
    return folder / f"{RUN_ID}-done-{seat}.txt"


def _seen_path(folder: Path, seat: str) -> Path:
    return folder / f"{RUN_ID}-seen-{seat}.txt"


def _shared_path(folder: Path, run_id: str | None = None) -> Path:
    return folder / f"{_rid(run_id)}-shared.txt"


# ── expected bodies (byte-exact, fully known to every seat) ───────────────────


def _hello(seat: str, run_id: str | None = None) -> str:
    return f"hello from {seat} in run {_rid(run_id)}\n"


def _ready(seat: str) -> str:
    return f"{seat} ready to edit in run {RUN_ID}\n"


def _done(seat: str, digest: str) -> str:
    return f"{seat} converged sha256={digest} in run {RUN_ID}\n"


def _seen(seat: str) -> str:
    """Phase 5a's barrier body. Writing this file means "my phase-4 wait
    returned", i.e. THIS seat has already materialised every peer's done file —
    which is exactly the fact a peer needs before it may delete its own."""
    return f"{seat} saw the whole cohort ack in run {RUN_ID}\n"


def _shared(
    state: dict[str, str],
    run_id: str | None = None,
    seats: tuple[str, ...] | None = None,
) -> str:
    """The shared file: one line per seat, separated by unique spacer lines so
    every pairwise diff3 hunk is a single-line replacement anchored by unique
    context — clean merges by construction, in any arrival order."""
    seats = SEATS if seats is None else seats
    lines = [f"multiseat shared file, run {_rid(run_id)}"]
    for n, seat in enumerate(seats):
        lines.append(f"{seat}: {state[seat]}")
        if n < len(seats) - 1:
            lines.append(f"spacer-{n}a")
            lines.append(f"spacer-{n}b")
    return "\n".join(lines) + "\n"


# ── small primitives ──────────────────────────────────────────────────────────


def _app_name_of(driver) -> str:
    """The `--client` name this driver was selected as."""
    for name, is_it in (
        ("tui", driver.is_tui),
        ("linux", driver.is_linux),
        ("macos", driver.is_macos),
        ("windows", driver.is_windows),
        ("web", driver.is_web),
    ):
        if is_it():
            return name
    return type(driver).__name__


def _seat_of(driver) -> str:
    """The seat THIS MACHINE plays — one machine, one seat, whichever client
    drives it.

    The seat names the MACHINE in the rendezvous (it prefixes this seat's run
    files and must match the cohort), so it is derived from the platform, never
    from the driver: the Linux machine is always ``linux``, the macOS machine
    ``macos``, the Windows machine ``windows``. The
    CLIENT is a ``--client`` parameter — a machine's seat may be driven by its
    native desktop app or, on unix, by the terminal client (both drive the
    same shared-Rust agent provisioner, and the tui dev layout even builds the
    agent as a sibling). That makes a tui seat a cheap way to tell "this
    machine's sync is broken" from "this machine's GUI app is broken".

    An unsupported client fails LOUDLY rather than skipping: a client with no
    local sync engine binds a folder with nothing behind it and looks bound while
    syncing in neither direction — precisely the failure that hid the broken
    windows and macOS seats behind a machine-global agent (2026-07-24).
    """
    err = cfg.seat_app_error(_app_name_of(driver), sys.platform)
    if err:
        pytest.fail(f"multiseat seat cannot run: {err}")
    return cfg.local_seat()


def _read(path: Path) -> str | None:
    """Current content, or None if absent. Tolerates a file mid-write (the
    engine promotes via rename, but the peer OS/agent may still race a read)."""
    try:
        return path.read_text()
    except OSError:
        return None


def _write(path: Path, content: str) -> None:
    """Atomic-enough write: temp sibling + rename, the replace-save shape every
    engine already handles (a watcher never observes a half-written body).

    The temp is **dot-prefixed** so the sync root treats it as hidden and never
    tries to sync it. Without that, this helper's own scaffolding entered the
    product under test: the watcher saw the temp appear, queued it as an upload,
    and by the time the debouncer drained, the rename had taken it away — one
    ``ERROR ... WS file change failed path=<temp> error=reading <temp>`` per
    write in every seat log — and the rename's ``Removed`` event then recorded a
    tombstone **on the nest** for a path no user ever had. Both were pure
    harness noise sitting in the middle of the failure diagnostics a session
    reads to debug real sync bugs (measured on macOS, 2026-08-05; the temp names
    hash to `path~872e328ad778` / `path~325bd1e0ada9` in that run's logs).

    Dot-prefixing is also the more faithful mimicry: the editors whose
    replace-save shape this imitates write hidden temps precisely so file
    watchers ignore them, and every seat already filters dotfiles.
    """
    tmp = path.with_name("." + path.name + ".tmp-multiseat")
    tmp.write_text(content)
    os.replace(tmp, path)


def _run_file_author(name: str) -> str | None:
    """The seat that owns a run file, from ``<ID>-<kind>-<seat>.txt``. The shared
    file has no single author (every seat edits it), so it returns None."""
    stem = name[:-4] if name.endswith(".txt") else name
    for s in _ALL_SEATS:
        if stem.endswith(f"-{s}"):
            return s
    return None


def _absent_diagnosis(name: str, seat: str, nest: set[str] | None, complete: bool) -> str:
    """Why a file is missing HERE, judged against the nest — never a bare
    "NEVER ARRIVED".

    That flat wording is a DETECTION BUG with a measured cost: it states a purely
    local fact ("not in my folder") in words that sound global ("the peer never
    wrote it"), and it has repeatedly sent sessions at the wrong machine —
    2026-07-24 alone, win's `-01` file was reported NEVER ARRIVED when it had
    merely landed after the window, and the linux seat was called the blocker
    while its own files were being deleted underneath it. The nest listing
    separates the three genuinely different cases.
    """
    author = _run_file_author(name)
    who = author or "a peer"
    if nest is None:
        return (
            "absent here; the nest listing could NOT be read, so 'never uploaded' "
            "vs 'uploaded but never delivered here' is UNDETERMINED — do not blame "
            "any machine on this line alone"
        )
    if name in nest:
        return (
            f"ON THE NEST but not delivered to this seat — nest -> {seat} is the "
            f"broken direction; {who}'s upload is NOT the suspect"
        )
    if not complete:
        return (
            f"not in this seat's folder, and not in a LOSSY nest read (some rows "
            f"unregistered) — most likely {who} -> nest never happened, but this "
            f"read cannot prove absence; re-check with a tui reader"
        )
    return (
        f"NOT ON THE NEST — {who} -> nest is the broken direction (or seat {who} "
        f"started late / never ran); this seat's download is NOT the suspect"
    )


def _still_present_diagnosis(
    name: str, seat: str, nest: set[str] | None, complete: bool
) -> str:
    """Why a peer's deleted file is STILL on this seat's disk — the mirror image
    of :func:`_absent_diagnosis`, and it names a side for the same reason.

    "The delete never propagated" is as local a fact as "the file never arrived",
    and phrasing it globally would re-enter the misdirection that function's
    docstring records. The nest listing separates the two genuinely different
    faults: the tombstone never reached the nest (the DELETER's record path), or
    it did and this seat never applied it (this seat's delete arm).

    **Which half of a lossy read is sound is the exact MIRROR of
    :func:`_absent_diagnosis`, and getting it backwards is how this function
    would invent an accusation.** There, a name READ is really on the nest
    (presence sound, absence not). Here the question is inverted — the file
    should be gone — so the sound observation is the name still being LISTED;
    its absence from a read that dropped rows proves nothing, and must stay
    hedged rather than hardening into "this seat failed to apply it"."""
    author = _run_file_author(name)
    who = author or "a peer"
    if nest is None:
        return (
            "still on disk here; the nest listing could NOT be read, so "
            "'the tombstone never reached the nest' vs 'it did and this seat "
            "never applied it' is UNDETERMINED — do not blame any machine on "
            "this line alone"
        )
    if name in nest:
        return (
            f"STILL ON THE NEST — {who} -> nest is the broken direction: {who} "
            f"never recorded the tombstone (or never reached phase 5). This seat's "
            f"apply path is NOT the suspect"
        )
    if not complete:
        return (
            f"still on this seat's disk and absent from a LOSSY nest read (some "
            f"rows unregistered) — most likely {who}'s tombstone DID land and this "
            f"seat never applied it, but this read cannot prove the nest dropped "
            f"it; re-check with a tui reader before blaming either side"
        )
    return (
        f"GONE FROM THE NEST but still on this seat's disk — {who}'s tombstone "
        f"landed, so nest -> {seat} apply is the broken direction and {who} is "
        f"NOT the suspect"
    )


def _own_upload_note(folder: Path, seat: str, nest: set[str] | None, complete: bool) -> str:
    """Am I the broken seat? Every phase wait is peers-only, so a seat can sit
    here blaming a peer while its OWN uploads never left the machine — the exact
    misdirection that cost three sessions. Judge my own written files against the
    nest and say so plainly."""
    mine_local = sorted(p.name for p in folder.glob(f"{RUN_ID}-*-{seat}.txt"))
    if not mine_local:
        return "  self-check: this seat has written no run files yet"
    if nest is None:
        return (
            f"  self-check: nest listing unreadable — whether MY OWN files "
            f"({', '.join(mine_local)}) ever uploaded is UNKNOWN"
        )
    missing = [n for n in mine_local if n not in nest]
    if not missing:
        return (
            f"  self-check: MY OWN files DID reach the nest ({', '.join(mine_local)}) "
            f"— this seat's upload path is healthy, so look at the peer named above"
        )
    if not complete:
        return (
            f"  self-check: MY OWN {', '.join(missing)} absent from a LOSSY nest read "
            f"— possibly this seat is a broken uploader; confirm with a tui reader"
        )
    return (
        f"  self-check: MY OWN {', '.join(missing)} NEVER REACHED THE NEST — "
        f"THIS seat ({seat}) is the broken uploader, and any peer reporting these "
        f"as missing is CORRECT. Fix this before suspecting a peer."
    )


def _report_arrivals(
    seat: str,
    phase: str,
    arrived: list[Path],
    pending: dict[Path, str],
    waited: float,
    *,
    verb: str = "arrived",
) -> None:
    """Narrate a wait WHILE it runs — name what landed and what is still out.

    Point 6 says a failure must diagnose itself; this is the same argument one
    step earlier, for a wait that has not failed yet. Phase 1's budget spans
    another machine's slot queue and cold build (up to 1200s), and the loop
    already knows on every poll which peers are outstanding — throwing that away
    until failure time is what forced run `20260730-02`'s operator to list the
    bound folder from outside the run to learn that only the macos seat was
    missing while the linux seat's hello and the merge base had both landed.

    Purely observational: no caller asserts on this, and the deadline poll above
    is unchanged, so convention 14 is untouched and a green run pays nothing.
    `flush=True` because the handshake's own notes record `-07` going blind for
    13 minutes when stdout was redirected to a file without PYTHONUNBUFFERED=1 —
    an operator forgetting an env var should not cost a round its live diagnosis.

    ``verb`` is what the observation MEANS — "arrived" for the phases waiting on
    content, "applied the delete of" for phase 5. One narrator with an honest
    verb beats a second copy that drifts, and beats one verb lying about half
    its callers (convention 7's complaint, one layer down).
    """
    for p in sorted(arrived, key=lambda q: q.name):
        print(
            f"[multiseat] {seat}: {phase} — {verb} {p.name} (+{waited:.0f}s)",
            flush=True,
        )
    still = ", ".join(sorted(q.name for q in pending))
    print(
        f"[multiseat] {seat}: {phase} — still waiting on: {still}"
        if still
        else f"[multiseat] {seat}: {phase} — all files observed (+{waited:.0f}s)",
        flush=True,
    )


def _await_files(
    seat: str,
    app,
    expectations: dict[Path, str],
    phase: str,
    budget: float | None = None,
    *,
    self_note=None,
    extra_note=None,
) -> None:
    """Poll until every path holds exactly its expected content, else fail with
    a per-file diagnosis naming the sync direction that broke (point 6: the
    failure must diagnose itself).

    `budget` defaults to WINDOW — the tight symmetric rendezvous budget. Phase 1
    passes ASSEMBLY_WINDOW instead, because it is the only wait that spans another
    machine's slot queue and cold build; see that constant's comment.

    `app` may be None — a seat with no client at all (the one-machine
    convergence legs, ``helpers/convergence_legs.py``). The nest listing and the app's
    error element are then simply unavailable, which the diagnosis already states
    honestly rather than guessing. Such a seat supplies its own, BETTER witness
    through the two hooks:

      `self_note(folder) -> str`   replaces the Media-listing self-check ("did MY
                                  files reach the nest?") with whatever evidence
                                  that seat actually has — for an app seat of
                                  the one-machine legs, its own disk and agent log,
                                  rather than a Media read-back.
      `extra_note() -> str`        appended verbatim; anything else worth having
                                  at failure time (log paths, applied files).

    Both default to None, so every existing caller is unchanged.
    """
    window = WINDOW if budget is None else budget
    started = time.monotonic()
    deadline = started + window
    pending: dict[Path, str] = dict(expectations)
    while time.monotonic() < deadline:
        arrived = [p for p, exp in pending.items() if _read(p) == exp]
        for p in arrived:
            del pending[p]
        if arrived:
            _report_arrivals(seat, phase, arrived, pending, time.monotonic() - started)
        if not pending:
            return
        time.sleep(1.0)
    # One nest read at failure time — the local folder cannot tell "the peer
    # never uploaded it" from "it is on the nest and never came down to me", and
    # naming the wrong side is how this suite has misdirected sessions before.
    nest, complete = _nest_listing(app)
    details = []
    for p, exp in pending.items():
        actual = _read(p)
        if actual is None:
            details.append(f"  {p.name}: {_absent_diagnosis(p.name, seat, nest, complete)}")
        else:
            details.append(
                f"  {p.name}: content mismatch\n"
                f"    expected: {exp!r}\n"
                f"    actual:   {actual!r}"
            )
    folder = next(iter(expectations)).parent
    note = (
        self_note(folder) if self_note is not None
        else _own_upload_note(folder, seat, nest, complete)
    )
    if app is None:
        witness = "  app error element: <no client on this seat>"
    else:
        try:
            err = app.error_text()
        except Exception:
            err = "<unreadable>"
        witness = f"  app error element: {err!r}"
    if extra_note is not None:
        witness = f"{witness}\n{extra_note()}"
    pytest.fail(
        f"[{seat}] {phase}: did not observe within {window:.0f}s "
        f"(FAUNA_MULTISEAT_TIMEOUT_SECS):\n" + "\n".join(details) +
        f"\n{note}"
        f"\n{witness}"
    )


def _await_deleted(
    seat: str,
    app,
    paths: list[Path],
    phase: str,
    budget: float | None = None,
    *,
    self_note=None,
    extra_note=None,
) -> None:
    """Poll until every path is GONE from this seat's folder.

    **The absence is sound here with no priming, and that is a property of the
    rendezvous rather than of this function.** An absence assertion needs a
    witnessed present→absent transition, or "deleted" and "never arrived"
    collapse into one observation. The one-machine cell has to buy that witness
    explicitly (``convergence_legs.prime_delete_witness``: its watchers are
    polled sequentially by a single process, so the second watcher's window can
    open after it has already applied the deletes). Here **phase 4 is the
    primer** — it waited on each of these files byte-exact ON THIS SEAT, so
    every file below was provably present here before its deleter unlinked it.

    **A timeout is DEFINITIVE, not slowness.** `file-sync.md` § *Deletes
    propagate the same way*: "the pull anchor advances to the batch's max seq
    whether or not a change was applied, so a tombstone declined once is
    excluded from every later pull". A tombstone this seat has not applied by
    the deadline will never be applied — so a red here is a real defect, and
    re-running it on a quieter machine is not a diagnosis.

    ``budget``/``self_note``/``extra_note`` carry exactly the meanings
    :func:`_await_files` gives them, so the engine twin's seat (no client, a
    better witness of its own) reuses this unchanged.
    """
    window = WINDOW if budget is None else budget
    started = time.monotonic()
    pending = list(paths)
    folder = paths[0].parent

    def _observe() -> bool:
        gone = [p for p in pending if not p.exists()]
        for p in gone:
            pending.remove(p)
        if gone:
            _report_arrivals(
                seat,
                phase,
                gone,
                {p: "" for p in pending},
                time.monotonic() - started,
                verb="applied the delete of",
            )
        return not pending

    def _diagnose() -> str:
        # One nest read at failure time only — `pending` now holds exactly what
        # is genuinely outstanding, so this never names a file it did not check.
        nest, complete = _nest_listing(app)
        details = [
            f"  {p.name}: {_still_present_diagnosis(p.name, seat, nest, complete)}"
            for p in pending
        ]
        note = (
            self_note(folder) if self_note is not None
            else _own_upload_note(folder, seat, nest, complete)
        )
        if app is None:
            witness = "  app error element: <no client on this seat>"
        else:
            try:
                err = app.error_text()
            except Exception:
                err = "<unreadable>"
            witness = f"  app error element: {err!r}"
        if extra_note is not None:
            witness = f"{witness}\n{extra_note()}"
        return (
            f"[{seat}] {phase}: the peer deletes never applied here "
            f"(FAUNA_MULTISEAT_TIMEOUT_SECS). A declined tombstone is excluded "
            f"from every later pull, so this will NOT resolve on a re-poll:\n"
            + "\n".join(details) +
            f"\n{note}"
            f"\n{witness}"
        )

    wait_until(_observe, window, interval=1.0, diagnose=_diagnose)


def _run_delete_phase(seat: str, folder: Path, peers: list[str], await_seen, await_deleted) -> None:
    """Phases 5a + 5b, ONE owner both seat modules call — because the ordering
    *is* the mechanism and an inline copy in each module could drift apart
    silently, which here means a live round hanging a peer forever.

    The safety property, stated so it can be tested: **the unlink below must be
    unreachable until ``await_seen`` has returned.** A seat writes its seen file
    only after its phase-4 wait returned, i.e. only once it holds every peer's
    done file; so by the time every peer's seen file is here, every peer holds
    mine and removing it can strand nobody. Reverse the two statements and the
    round is back to the hazard the module docstring's cleanup bullet describes —
    a tombstone that wins batch-latest on a seat which never materialised the
    file, whose phase-4 wait then cannot be satisfied by anything.

    The two waits are injected rather than called directly: the client seat
    diagnoses failures through the Media listing, the engine seat through its
    daemon's own log, and neither should have to know the other exists.
    """
    _write(_seen_path(folder, seat), _seen(seat))
    print(f"[multiseat] {seat}: phase 5a — cohort ack seen, awaiting peers", flush=True)
    await_seen({_seen_path(folder, p): _seen(p) for p in peers})

    mine = _done_path(folder, seat)
    mine.unlink()
    print(
        f"[multiseat] {seat}: phase 5b — deleted my own {mine.name}, awaiting "
        f"the peers' deletes to apply here",
        flush=True,
    )
    await_deleted([_done_path(folder, p) for p in peers])


# ── UI steps (shared cross-app action layer; no per-seat branches) ─────────


def _sign_in(
    app,
    seat: str,
    *,
    secret_hex: str | None = None,
    address: str | None = None,
) -> None:
    """Import the account seed, run the REAL DoH handle check, sign in. Never
    claims: three concurrent seats racing a claim would clobber each other, so
    an unclaimed nest is a precondition failure, not something to fix here.

    ``secret_hex``/``address`` are additive overrides carrying the same contract
    as the run-file protocol's ``run_id``/``seats`` overrides: **the defaults
    reproduce the tri-machine round's own behaviour exactly**, so this stays one
    owner rather than growing a copy. The nest-mode axis's seat module passes
    both under ``--nest live``, where the account is a FRESH harness-provisioned
    one rather than the shared long-lived seed — same flow, different identity
    (`tests/test_filesync_seats.py::live_sign_in`, which owns the explanation
    of why a seconds-old account can use this production path at all).
    """
    secret_hex = secret_hex or SECRET
    address = address or ADDRESS
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    ob.fill_handle(address)
    ob.run_handle_check(timeout=60)
    ob.submit_handle()

    deadline = time.monotonic() + 150.0
    while time.monotonic() < deadline:
        if app.driver.is_visible("feed-view") or app.driver.is_visible("feed-tab"):
            return
        if app.driver.is_visible("claim-code-input"):
            pytest.fail(
                f"[{seat}] the nest at {address!r} is UNCLAIMED. This test never "
                "claims (three seats would race the claim) — claim the box once "
                "from one client first, then rerun all three seats."
            )
        if app.driver.is_visible("launch-retry-button"):
            try:
                app.driver.click("launch-retry-button")
            except Exception:
                pass
        time.sleep(2.0)
    pytest.fail(
        f"[{seat}] sign-in never reached the feed within 150s; "
        f"error={app.error_text()!r}"
    )


def _wait_connected(app, seat: str, timeout: float = 90.0) -> None:
    """The authed shell renders before the WS-RPC connect completes; folder
    RPCs before Connected would hit `rpc disconnected`."""
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            last = app.driver.get_text("connection-status") or ""
        except Exception:
            last = ""
        if last.startswith("Connected"):
            return
        time.sleep(1.5)
    pytest.fail(
        f"[{seat}] WS-RPC never reached Connected within {timeout:.0f}s "
        f"(last status: {last!r})"
    )


def _read_set_listing(app) -> tuple[list[str], int]:
    """Enumerate the ``e2e-multiseat`` set's file basenames THROUGH the Media
    explorer UI (never a raw API — the read is itself the production-code test).

    FRESH BY CONSTRUCTION: every call re-enters the page
    (:meth:`MediaActions.reenter` — toggle ``feed`` → ``media``), because the
    Media listing pulls ``fauna.media.list`` only on the navigation EDGE
    (tui ``App::apply`` fires the refresh on ``page != prior`` only; linux on
    ``connect_map``) and ``set_filter`` is pure render state. A read that does
    not force its own edge returns the *previous* read's world — which is how
    run ``20260724-07`` false-red-ed: the own-ack barrier re-navigated to a
    page it was already on, so 600 s of "polling" re-read one frozen snapshot
    while the ack it was polling for was provably nest-visible mid-window.

    Best-effort scopes to the set via ``media-folder-filter``; the filter only
    lists sets that already have files (media.md § O-4), so on a brand-new/empty
    set the select is a no-op and the default all-media view is read instead —
    either way our uniquely-named ``YYYYMMDD-NN-*`` run files are in the list,
    and :func:`multiseat_config.next_run_id_from_names` greps them. The list
    loads asynchronously (``MediaMachine`` pulls ``fauna.media.list``), so poll
    until the names stabilise.

    COMPLETENESS HAS TWO AXES, and this function is responsible for both:

    * **Freshness** — handled above, by construction (the forced edge).
    * **Registration** — TOLERATED and reported: some clients register only
      on-screen rows (a lazy list), so an off-screen ``media-item``'s name
      element 404s once the set has enough files to scroll (the macOS/windows
      Media lazy-list gap, being closed apple-side — same class as the
      search/backups eager-ScrollView fix). We keep whatever names ARE
      readable and carry the row count so callers can tell a lossy read from a
      complete one. The terminal client registers every row of its snapshot
      (``media/mod.rs::elements`` iterates all items — no viewport clipping),
      which is why tui is the reliable announce reader; a lazy-list client
      would under-count NN. Note the default Name-ascending sort places our
      date-prefixed run files at the TAIL of a growing set, so a lazy list is
      structurally biased to miss exactly the newest round's files.

    An EMPTY listing is never settled early — see
    :func:`multiseat_config.settle_listing`. Sameness alone is not settledness: a
    listing that has not loaded yet also reads ``[]``, and two consecutive ``[]``
    reads are indistinguishable from a stable empty set, so the old loop returned
    ``[]`` after ~1 s. That silently yields run id ``-01`` regardless of what is
    really on the nest, and neither the announce nor phase 0 can catch it (phase 0
    checks only its OWN seat's files). Empty therefore polls to the deadline and
    warns; only a NON-EMPTY listing settles early.

    ⚠ Do NOT reintroduce "the filter proved the set is non-empty" as a floor. That
    was tried 2026-07-24 and is unsound: ``set_filter(SET_NAME)`` SUCCEEDS against a
    genuinely empty set (media.md § O-4's "only sets that have files" describes the
    offered options, but the select does not validate the value on this path), so it
    raised on a correct empty read.

    ``media-empty-state`` (ui.yaml `media` page, user-approved 2026-08-05) is the
    SOUND version of that instinct — the app stating "I finished reading and found
    nothing", rather than the harness inferring it from a filter or a stopwatch.
    Every app paints it now (:meth:`MediaActions.has_loaded`, all 7 apps as of
    2026-08-06; see ``media.md`` § Implementation status today), so an empty
    listing is always settled at once and a never-loading one is always a raised
    diagnosis instead of a silent ``[]``.

    RETURNS ``(names, rows)`` — the names that could be read, and how many rows the
    client believed existed at the final snapshot. ``rows > len(names)`` means the
    read was LOSSY (the lazy-list gap above), and that distinction is load-bearing
    for any caller reasoning about ABSENCE: a name missing from a lossy read may
    simply not have been registered. Presence is always sound; absence is only
    sound from a complete AND fresh read — ``rows`` proves nothing about
    freshness (a frozen snapshot's count is as stale as its names), which is why
    the edge is forced here rather than trusted to the caller's poll loop.
    See :func:`_nest_listing`."""
    m = app.media
    m.reenter()
    try:
        # offer_timeout=0: best-effort — "not offered" is an ANSWER here (the
        # other seat's set may not have synced yet), not a latency to wait out,
        # and this helper runs inside poll loops that cannot afford the default
        # offer budget per call.
        m.set_filter(SET_NAME, offer_timeout=0)
    except Exception:
        pass  # empty/new set has no filter option; the all-media view still lists

    # ORDER the listing newest-first, so the rows that decide the run id sit at
    # the HEAD — which even a lazy-list client registers. This converts a
    # COMPLETENESS requirement (unenforceable from the harness: an off-screen row
    # is indistinguishable from an absent one, and `item_count()` counts the same
    # rows that failed to materialise) into an ORDERING one, which is a single UI
    # action. Under the default Name-ascending sort our date-prefixed run files
    # land at the TAIL of a growing set, so a lazy list is structurally biased to
    # miss exactly the newest round's files — the failure that under-counts NN,
    # yields a lower id, and strands that machine in a cohort that never meets it.
    #
    # Best-effort like the filter, but NOT silent: a client that has not grown
    # `media-sort-direction` yet still reads correctly (just with the old
    # tail-bias), and says so rather than looking ordered when it isn't
    # (testing.md point 6 — a failure must diagnose itself).
    try:
        m.set_sort("date")
        m.set_sort_direction("descending")
    except Exception as e:
        print(
            "[multiseat] WARNING: could not order the listing newest-first "
            f"({e!r}); falling back to the client's default order. On a "
            "lazy-list client the newest rows may be off-screen and unread."
        )

    # Rows the client BELIEVES exist at the last snapshot — the completeness
    # yardstick. A name we could not read is invisible in `names`, which makes it
    # indistinguishable from a file that genuinely is not on the nest; carrying
    # the count is what lets a caller tell "absent" from merely "unread".
    seen_rows = [0]

    def _snapshot() -> list[str]:
        out: list[str] = []
        count = m.item_count()
        seen_rows[0] = count
        for i in range(count):
            try:
                out.append(m.item_name(i))
            except LookupError:
                continue  # off-screen/unregistered row on a lazy-list client
        return out

    # The app's own loaded-vs-loading answer (`media-empty-state`, all 7 apps as
    # of 2026-08-06) turns "empty" from a 20 s guess into a fact, and a
    # never-loading read from a silent `[]` into a raised diagnosis above.
    names = cfg.settle_listing(_snapshot, loaded=m.has_loaded)
    if not names:
        loaded = m.has_loaded()
        if loaded is True:
            print(
                "[multiseat] the Media listing is EMPTY and the app says so "
                "(media-empty-state is painted) — a proven empty set, so the next "
                "run id is -01. No verification needed."
            )
        else:
            # Should not happen: settle_listing raises above once has_loaded()
            # is observed False after the deadline. Reaching here means the
            # probe flipped between settle_listing's last check and this one —
            # kept as a defensive print rather than trusting a silent `[]`.
            print(
                "[multiseat] WARNING: the Media listing is EMPTY after polling to "
                "the deadline, and a re-check of has_loaded() no longer says "
                "True. If the set really is empty this is correct and the next "
                "run id is -01; if it is not, the client's fauna.media.list read "
                "did not load and the computed id will be WRONG. Verify from "
                "another client before `go`."
            )
    return names, seen_rows[0]


def _read_set_item_names(app) -> list[str]:
    """Just the names — the announce and phase 0 don't reason about absence."""
    return _read_set_listing(app)[0]


def _nest_listing(app) -> tuple[set[str] | None, bool]:
    """The NEST's own view of the set: ``(names, complete)``, or ``(None, False)``
    if the Media read did not complete at all.

    The bound folder only ever answers "did this reach THIS machine". Which SIDE
    of the sync broke is a different question, and within a seat run this listing
    is the only witness that can answer it.
    """
    try:
        names, rows = _read_set_listing(app)
    except Exception:
        return None, False
    return set(names), len(names) >= rows


def _folder_index(b, set_name: str | None = None) -> int | None:
    wanted = SET_NAME if set_name is None else set_name
    for i in range(b.folder_count()):
        if wanted in b.folder_title(i):
            return i
    return None


# The budget a SINGLE Folders mount gets to populate its row list before a read
# of that list means anything. Named, generous, and paid only by a red run — a
# settled page returns on the first poll (convention 14).
ROW_SETTLE_BUDGET = 20.0


def _await_folder_rows(b, budget: float = ROW_SETTLE_BUDGET) -> None:
    """Give THIS mount of the Folders page time to populate its row list.

    ⚠ `folder-add-button` is NOT a readiness signal for the rows. It lives in the
    section HEADER and renders synchronously with the page; the rows arrive from
    the machine's first async `refresh()`. Measured on macOS against the live
    account (2026-07-31, three consecutive mounts): the add button is present at
    **0.00 s** with **0** rows, and the five rows register **0.21 s** later. So a
    read taken "once the add button is up" is a read of the pre-refresh page.

    This is what made the macOS seat look like it could not see the shared set at
    all — and it cost two operator-coordinated tri-machine rounds plus three
    sessions of diagnosis aimed at the nest, the MLS join-filter, and apple's
    `list_folders`, none of which were ever at fault. The instrumented run that
    settled it recorded `refresh: snapshot committed folders=5` and
    `vm sees folders=5` while the harness counted 0.

    Clients differ legitimately here and BOTH shapes are correct: tui builds its
    `DevicesMachine` once at session attach, so its rows are in the snapshot
    before the page ever paints (which is why tui always passed); macOS/iOS build
    a fresh per-mount `DevicesMachineVM` (`@State` in `MacFoldersView`) and pay
    one async round trip per mount. The test must not encode either client's
    timing — it must poll for the state it asserts.

    Deliberately does NOT re-enter the page: a re-entry REMOUNTS, which discards
    the in-flight load and restarts the very clock this is waiting on. Re-entry
    to pick up a *peer's* newly created set is a separate, slower loop
    (`_adopt_or_create_set`) layered on top of this one.
    """
    deadline = time.monotonic() + budget
    while True:
        try:
            if b.folder_count() > 0:
                return
        except Exception:  # noqa: BLE001 — a read failure is just "not ready yet"
            pass
        if time.monotonic() >= deadline:
            return  # the caller's own assertion reports the empty list
        time.sleep(0.2)  # sleep-ok: poll cadence of the deadline loop above; nothing is asserted about this interval and the loop exits the moment a row registers


def _rendered_folder_titles(b) -> list[str]:
    """Every folder row title this client currently renders — the single most
    diagnostic fact when :func:`_folder_index` comes back empty, and the one the
    old failure message threw away.

    Note WHY the count matters as much as the titles: ``folder_count()`` is
    ``driver.count("folder-row")``, which counts *registered* automation slots,
    and a row registers only when its view is actually built. So a client that
    realizes rows lazily loses the row AND the count together — the loop in
    :func:`_folder_index` never even visits the missing index, and no
    ``LookupError`` is ever raised. An empty list here is therefore the smoking
    gun for a registration/realization fault, not for an absent set. (Exactly the
    ``media-item`` fate-sharing already documented for the Media grid.)

    Defensive by design: a row counted but unreadable is reported as such rather
    than raising out of the diagnosis it exists to produce."""
    try:
        n = b.folder_count()
    except Exception as exc:  # noqa: BLE001 — diagnosis must never raise
        return [f"<row count unreadable: {exc!r}>"]
    out: list[str] = []
    for i in range(n):
        try:
            out.append(b.folder_title(i))
        except Exception as exc:  # noqa: BLE001
            out.append(f"<row {i} unreadable: {exc!r}>")
    return out


def _diagnose_missing_set_row(app, b, seat: str, waited_s: float | None) -> str:
    """Why is :data:`SET_NAME` absent from this client's Folders row list?

    **Diagnose against the NEST, never against the peer.** The old message asked
    "was the {CREATOR} seat started?", which names the one party this seat has no
    evidence about — and on run ``20260730-02`` that accusation was provably
    FALSE: the creator had created the set and both other machines had already
    exchanged files through it, while this seat sat 1200 s blaming it. Same class
    as the "NEVER ARRIVED" misdiagnosis this module already closed once.

    The witness that settles it is :func:`_nest_listing` — the SAME nest state
    read through a DIFFERENT client surface (the Media explorer). If Media
    returns files for the set, the set demonstrably exists and the creator
    demonstrably ran, so the broken surface is this client's own row list.

    ⚠ The empty case stays deliberately weak: ``set_filter`` succeeds against a
    genuinely empty set, so "no files" is consistent with both a creator that
    never ran and a real empty set. Claiming more would just relocate the old
    false accusation (see :func:`multiseat_config.settle_listing`'s own note)."""
    rendered = _rendered_folder_titles(b)
    nest_names, _complete = _nest_listing(app)
    if nest_names:
        verdict = (
            f"THE FAULT IS LOCAL TO THIS CLIENT. Its own Media surface completed "
            f"a read and returned {len(nest_names)} file(s) — so this client is "
            f"signed in, connected, and rendering listings — while the Folders "
            f"row list above rendered {len(rendered)}. Two different surfaces of "
            f"ONE client over ONE nest: a dead connection cannot produce that "
            f"split, and neither can the {CREATOR} seat. Do NOT re-run the "
            f"cohort to learn this; suspect this client's folder list read and "
            f"render (a lazily-realized row costs the count as well as the row, "
            f"so 0 rows raises no exception to mark it).\n"
            f"  ⚠ NOT proof the SET exists: set_filter is best-effort and falls "
            f"back to the ALL-MEDIA view when {SET_NAME!r} offers no filter "
            f"option — which is precisely what an empty folder list causes — "
            f"so this witness cannot separate 'the set's files' from 'the "
            f"account's files'. To settle the set's existence, read it from a "
            f"DIFFERENT client on this same machine (--client tui)."
        )
    elif nest_names is None:
        verdict = (
            f"CANNOT TELL from this seat: the Media read of {SET_NAME!r} did not "
            f"complete at all, so neither the set's existence nor the {CREATOR} "
            f"seat's start is established here. Check this client's connection "
            f"before suspecting any peer."
        )
    else:
        verdict = (
            f"the Media read of {SET_NAME!r} completed but returned NO files. "
            f"That is consistent with the {CREATOR} seat never having created or "
            f"written the set — but ALSO with a genuinely empty set, since "
            f"set_filter succeeds on one. Not proof either way; confirm from "
            f"another client before concluding a peer failed."
        )
    waited = (
        f" within {waited_s:.0f}s" if waited_s is not None else ""
    )
    return (
        f"[{seat}] folder {SET_NAME!r} never appeared in THIS CLIENT'S "
        f"Folders row list{waited}.\n"
        f"  rows rendered here: {len(rendered)} -> {rendered}\n"
        f"  nest witness: {verdict}\n"
        f"  error={app.error_text()!r}"
    )


def _adopt_or_create_set(app, b, seat: str, set_name: str | None = None) -> None:
    """Reach the shared set's row. Single deterministic creator: the CREATOR seat
    (canonical-first present in the cohort) creates the set (sync mode, 60 s
    cadence — the catalog minimum, so a missed watcher event still reconciles
    inside one wait window); the others only ever adopt, polling with page
    round-trips (a fresh mount re-fetches the list) until the creator's create —
    or a previous run's set — shows up.

    A non-creator's wait here is budgeted at ASSEMBLY_WINDOW, not the tight
    WINDOW: it is the SAME asymmetric wait phase 1 has (a peer cannot hurry the
    creator's output), just one step earlier — before the folder is even bound.
    Live proof, run 20260730-01: the macos seat failed exactly this wait at
    600s ("was the linux seat started?") while linux's own seat build was still
    running, the identical failure mode phase 1's ASSEMBLY_WINDOW exists to
    prevent.

    ``set_name`` is the two-seat override (convention 16). Omitted, every line
    below is byte-for-byte the tri-machine behaviour. Supplied, the caller has
    ALREADY created the set (the two-seat local twin creates it over the admin
    RPC before any seat starts; the live twin adopts the long-lived shared set),
    so this becomes adopt-only: there is no cohort here, `CREATOR` names a
    MACHINE seat that two same-machine seats never match, and a seat that
    "created" the set because its row had not rendered yet would be the
    duplicate-set bug the settle above exists to prevent."""
    wanted = SET_NAME if set_name is None else set_name
    b.navigate_folders()
    app.driver.wait_for("folder-add-button", timeout=30)
    # Settle THIS mount before deciding the set is absent. On the CREATOR seat
    # this check gates a `create_folder_via_wizard`, so reading the page before
    # its rows load does not merely mis-report — it drives the creator to
    # re-create a set that already exists.
    _await_folder_rows(b)
    if _folder_index(b, set_name) is not None:
        return
    if set_name is None and seat == CREATOR:
        b.create_folder_via_wizard(wanted)
        assert _folder_index(b, set_name) is not None, (
            f"created folder {wanted!r} but its row never rendered; "
            f"error={app.error_text()!r}"
        )
        return
    deadline = time.monotonic() + ASSEMBLY_WINDOW
    while time.monotonic() < deadline:
        b.navigate_devices()
        b.navigate_folders()
        app.driver.wait_for("folder-add-button", timeout=15)
        # Was a flat `sleep(2.0)` — enough for the ~0.2 s macOS mount in practice,
        # but a fixed delay is exactly what convention 14 forbids: it gives a
        # green run a guaranteed 2 s tax and a loaded box no headroom at all.
        # Polling for the observed row list costs a settled page one pass and
        # survives a slow one.
        _await_folder_rows(b)
        if _folder_index(b, set_name) is not None:
            return
    if set_name is None:
        pytest.fail(_diagnose_missing_set_row(app, b, seat, ASSEMBLY_WINDOW))
    # The two-seat path diagnoses against what THIS seat can see. The tri-machine
    # diagnosis above reads the live shared set through the Media UI, which would
    # be a read of the wrong set entirely here.
    rendered = [b.folder_title(i) for i in range(b.folder_count())]
    pytest.fail(
        f"[{seat}] folder {wanted!r} never appeared in this seat's Folders "
        f"row list within {ASSEMBLY_WINDOW:.0f}s. The set is created before any "
        f"seat starts, so a red here is the seat's own nest connection or its "
        f"row list, not a peer.\n"
        f"  rows rendered here: {len(rendered)} -> {rendered}\n"
        f"  error={app.error_text()!r}"
    )


def _bind_folder(app, b, seat: str, folder: Path, set_name: str | None = None) -> None:
    """Bind the local folder under the set's expander (the nested binding form:
    the set is contextual, no free-text set name). On linux/macos this starts
    the in-process engine immediately; on windows it lands over the named pipe
    in the real fauna-sync-agent.exe agent (real_sync_agent) in resident mode
    (on-demand is an explicit toggle this test never touches).

    ``set_name`` is the two-seat override (convention 16); omitted, the behaviour
    is byte-for-byte the tri-machine one."""
    wanted = SET_NAME if set_name is None else set_name
    # Expanding is NOT a one-shot action, and waiting longer does not help: the
    # Folders page mounts its rows (so `folder-add-button` is visible and the
    # titles are readable) BEFORE each row's member roster finishes loading from
    # the nest — the titles literally read "Loading members...". When that async
    # load lands it REBUILDS the row, discarding an expander click that arrived
    # first. The click is swallowed, not merely slow, so a bare `wait_for` on the
    # nested input observes nothing however long it waits (it timed out at 30 s
    # with count=0 on the live 2026-07-24 run, while the identical call worked
    # whenever the page happened to have settled first).
    #
    # So: click, look for the nested form, and re-click if the row rebuilt under
    # us. Latency-independent per testing.md point 14 — a generous deadline polled
    # for OBSERVED state, which costs an already-settled page a single pass.
    deadline = time.monotonic() + max(60.0, WINDOW / 4)
    while True:
        b.find_and_expand_folder(wanted)
        settle = time.monotonic() + 5.0
        while time.monotonic() < settle:
            if app.driver.count("folder-location-path-input") >= 1:
                break
            time.sleep(0.25)
        if app.driver.count("folder-location-path-input") >= 1:
            break
        if time.monotonic() >= deadline:
            pytest.fail(
                f"[{seat}] the {wanted!r} expander never opened its binding form "
                f"(folder-location-path-input count=0 after repeated expand clicks over "
                f"{max(60.0, WINDOW / 4):.0f}s). The row is present (index lookup "
                f"succeeded), so this is the mount-vs-member-load rebuild race, not "
                f"a missing set. error={app.error_text()!r}"
            )
    app.driver.type_text("folder-location-path-input", str(folder))
    app.driver.click("folder-location-add-button")
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if app.driver.count("folder-location-row") >= 1:
            return
        time.sleep(0.5)
    pytest.fail(
        f"[{seat}] bound folder never rendered as a folder-location-row; "
        f"error={app.error_text()!r}"
    )


def _unbind_folder(app, b) -> None:
    """Best-effort teardown: stop this seat's engine watching the (about to be
    reclaimed) tmp folder. Matters most on windows, where the detached agent
    outlives the test session."""
    try:
        b.navigate_folders()
        b.find_and_expand_folder(SET_NAME)
        if app.driver.count("folder-location-row") >= 1:
            app.driver.click("folder-location-remove-button")
            time.sleep(2.0)
    except Exception:
        pass


def _await_own_ack_on_nest(app, seat: str) -> None:
    """Block until THIS seat's phase-4 ack is visible ON THE NEST.

    Every other wait in phases 1–4 is peer-only: a seat proves it saw the
    others, never that the others can see IT. Phases 1–3 survive that asymmetry
    on latency luck alone — the peer wait happens to outlast this seat's own
    upload — which is exactly the wall-clock coupling convention 14 forbids
    relying on. In phase 4 the luck runs out: the LAST seat to arrive finds
    every peer ack already present, so its peer wait returns almost immediately
    and teardown reaps the agent before the watcher → record → upload cycle
    drains its own ack.

    Live proof, run ``20260724-06`` (mac seat, three-seat cohort): this seat
    printed ``cohort converged … — PASS`` while ``20260724-06-done-macos.txt``
    never reached the nest — confirmed absent by two announce-reader reads
    minutes apart, with all nine other run files present. Both peers block on
    exactly that file, so a seat can pass while starving the cohort, and the
    peers then fail phase 4 naming THIS machine. That is the same peer-only
    blind spot that made a linux-local deletion present as "windows never
    arrived" and misdirected three sessions.

    Latency-independent per convention 14: a deadline poll on nest-visible
    state, never a settle sleep. A green run pays a single Media read.

    ⚠ EVERY POLL MUST BE A FRESH READ, and the seat this barrier exists for is
    exactly the one a stale read betrays. The Media listing pulls the nest only
    on a navigation EDGE, and the last-finishing seat writes its own ack
    *seconds* before this barrier's first poll — so with edge-less polling the
    upload always loses that race and the remaining window re-reads one frozen
    snapshot. That is run ``20260724-07``'s false red: mac's ack was provably
    nest-visible with 5+ minutes of "polling" left, and the barrier never saw
    it, failing a healthy run — the most expensive failure mode this test has.
    :func:`_read_set_listing` now forces the edge on every call, so each loop
    iteration here is a genuine re-pull (gate:
    ``test_the_run_20260724_07_frozen_listing_must_not_red_the_barrier``).

    ⚠ PRESENCE is sound on every app; ABSENCE is only sound from a COMPLETE
    read (:func:`_read_set_listing`). A lazy-list GUI seat registers only
    on-screen rows, so an unread ack is indistinguishable from an absent one —
    and failing there would red a CORRECT run just like the frozen read did.
    So a lossy final read ABSTAINS with a loud UNCERTIFIED warning rather than
    asserting. The terminal client registers every row of its snapshot (no
    lazy list), so a tui seat never takes the abstain path — ``-07`` proved
    the freshness axis, not the registration axis, is where a tui read goes
    stale.
    """
    want = f"{RUN_ID}-done-{seat}.txt"
    deadline = time.monotonic() + WINDOW
    last: object = "<no successful read>"
    # State of the LAST read: "complete" (absence is provable) / "lossy"
    # (absence is unprovable) / "failed" (the read itself did not complete).
    # Only the final read decides — one early unregistered row must not excuse
    # a genuine starvation for the rest of the window.
    read_state = "failed"
    while True:
        try:
            readable, rows = _read_set_listing(app)
        except Exception as e:  # transient list/RPC hiccup — keep polling
            last = f"<Media read failed: {e!r}>"
            read_state = "failed"
        else:
            names = set(readable)
            read_state = "complete" if len(readable) >= rows else "lossy"
            if want in names:
                print(f"[multiseat] {seat}: phase 4 — own ack confirmed on the nest")
                return
            last = sorted(n for n in names if n.startswith(f"{RUN_ID}-"))
        if time.monotonic() >= deadline:
            break
        time.sleep(2.0)  # sleep-ok: poll cadence of a deadline poll (convention 14 mechanism 1), not a settle wait — the loop returns the instant the ack is visible, so a green run never reaches this line

    if read_state == "lossy":
        print(
            f"[multiseat] {seat}: phase 4 — own-ack barrier UNCERTIFIED. "
            f"{want!r} was not among the names this client could read, but the "
            f"read was LOSSY (a lazy-list client registers only on-screen rows), "
            f"so its absence is NOT proven and this seat will not claim it "
            f"starved the cohort. If a peer fails phase 4 naming this machine, "
            f"THIS is the seat to investigate first. Re-read the set from a tui "
            f"client to settle it. Last readable {RUN_ID} listing: {last}"
        )
        return

    raise AssertionError(
        f"[{seat}] my own ack {want!r} never reached the nest within "
        f"{WINDOW:.0f}s (last readable {RUN_ID} listing: {last}). Every PEER "
        f"wait passed, so this seat converged locally — but the other seats "
        f"block on this file and will fail phase 4 naming THIS machine. "
        f"Suspect this seat's agent being torn down before it drained the "
        f"upload, or the upload failing silently."
    )


def _assert_run_id_fresh(app, seat: str) -> None:
    """Phase 0 freshness self-check: before writing anything, read the set
    through the Media explorer and prove THIS seat's own ``<ID>-*-<seat>`` files
    aren't already present — i.e. the session didn't hand us a stale/reused id.
    Peers' files may already be present (all three start ~together); only MY OWN
    files being present means the id was reused. Also gives the Media UI real
    coverage on every GUI seat.

    Best-effort: this is a pre-flight safety net, NOT the behavior under test —
    the sync phases below read files by direct path, not via the Media UI. So if
    the Media read can't complete on this client (a lazy-list registration gap,
    a transient RPC hiccup), SKIP the check rather than fail the sync run."""
    try:
        names = set(_read_set_item_names(app))
    except Exception as e:
        print(
            f"[multiseat] {seat}: phase 0 self-check SKIPPED "
            f"(Media read did not complete: {e!r})"
        )
        return
    mine = {
        f"{RUN_ID}-hello-{seat}.txt",
        f"{RUN_ID}-ready-{seat}.txt",
        f"{RUN_ID}-done-{seat}.txt",
    }
    clash = sorted(mine & names)
    assert not clash, (
        f"[{seat}] run id {RUN_ID!r} is STALE — my own files already exist in "
        f"{SET_NAME!r}: {clash}. The announce computes a fresh id; a reused id "
        "means a manual FAUNA_MULTISEAT_RUN_ID override or a duplicated `go`. "
        "Re-run the announce for a fresh id, then retry."
    )


# ── the tests ─────────────────────────────────────────────────────────────────


def _ensure_local_seat_built() -> None:
    """Build THIS machine's seat client, so announcing implies "ready to `go`".

    The announce runs on the reader client, but the id it prints is the
    operator's cue to `go` on EVERY machine simultaneously. A seat that only
    starts building then holds the whole cohort hostage: the other seats' phase
    waits are capped at ``WINDOW`` (default 180 s) while a cold seat build is
    ~10 min, so they fail `NEVER ARRIVED` on a seat that was merely late.

    Measured 2026-07-24 (win): the id was announced with only the *reader* built
    and the cohort had to abort mid-run — the reason this is a code gate rather
    than a step in the handshake docs. Warm builds are a no-op, so a machine that
    did its warm build during `next` pays nothing here.
    """
    if cfg.local_seat() is None:  # not a seat platform — the reader still announces
        return
    client = cfg.seat_app(sys.platform)
    err = cfg.seat_app_error(client, sys.platform)
    if err:
        pytest.fail(f"FAUNA_MULTISEAT_SEAT_CLIENT is unusable here: {err}")
    from conftest import _ensure_app_built

    _ensure_app_built(client)


def test_announce_next_run_id(app):
    """READER step of the handshake (run during `next`, any client — default the
    terminal client on all three machines). Builds this machine's seat client,
    signs in, reads the ``e2e-multiseat`` set through the Media UI, and prints
    the next run id as ``Ready to start RUN_ID=...``. A genuine end-to-end test
    of the reader client's sign-in + WS connect + Media navigation + set-scoped
    enumeration against the live nest (testing.md point 8 — no wasted setup).

    The seat build comes FIRST and the set read LAST: the printed id must be as
    fresh as possible relative to the print, and no cohort-blocking work may
    remain after it (see :func:`_ensure_local_seat_built`)."""
    _ensure_local_seat_built()
    _sign_in(app, "reader")
    _wait_connected(app, "reader")
    names = _read_set_item_names(app)
    # Report WHAT was observed, not just the derived id: when a round fails to
    # rendezvous, the operator's first question is "whose files actually reached
    # the nest?" — and this reader is the only window into the shared set. Naming
    # today's run files separates "a peer never started / used another id" from
    # "the peer wrote but nest -> this seat never delivered" without a second run.
    today = cfg.today_str()
    todays = sorted(n for n in names if n.startswith(today))
    print(
        f"[multiseat] reader saw {len(names)} readable file(s) in {SET_NAME!r}; "
        f"today's run files: {todays if todays else '(none)'}"
    )
    run_id = _resolve_run_id(cfg.next_run_id_from_names(names, today))
    assert re.fullmatch(r"[A-Za-z0-9_-]{1,40}", run_id), (
        f"computed run id {run_id!r} is not a valid id"
    )
    # Hand the id to a seat test running in THIS pytest session, so the operator
    # may collapse announce+seat into one invocation and pay the machine-wide
    # `e2e` slot queue once instead of twice. Instant (two assignments), so the
    # `Ready to start` print below still comes last — nothing cohort-blocking may
    # follow it (see `_ensure_local_seat_built`).
    _publish_run_id(run_id)
    # Record that THIS id came out of a nest read, not out of an env var — the
    # only thing that makes it fresh by construction (see the flag's comment).
    _note_announced_in_session()
    print(f"Ready to start RUN_ID={run_id}")


def test_preflight_this_seat_sees_the_shared_set(app):
    """PREFLIGHT — ONE seat, no cohort, no writes, no run id. Does the client
    this machine will seat with actually render the shared set's row?

    **Run it with the SAME ``--client`` the seat will use**, e.g.::

        pytest tests/test_filesync_multiseat_live.py::test_preflight_this_seat_sees_the_shared_set \\
            --client macos -s

    Why this is NOT folded into the announce (which already signs in and reads
    Media): the announce's client is a free PARAMETER — the reader is normally
    ``tui`` while the seat is the machine's native app — so an announce-side
    check would have exercised the wrong client exactly when it mattered. On run
    ``20260730-02`` the tui reader was fine and the NATIVE macos seat was the
    surface that could not see the row; only a check running AS the seat client
    catches that.

    What it costs vs. what it saves: one sign-in against the live nest, no
    mutation of any kind. It converts a whole operator-coordinated tri-machine
    round — three humans' machines, ~30 min, and a failure that reads as a peer's
    fault — into a single-machine question answered before anyone types `go`.
    Both failed rounds this file records (``-01`` and ``-02``) died on this seat's
    inability to reach the set's row, one step apart, and neither needed a cohort
    to discover.

    Not a substitute for a round: it proves this client can SEE the set, never
    that sync converges."""
    seat = _seat_of(app.driver)
    print(f"[preflight] seat={seat} set={SET_NAME!r} — sign-in, then the Folders row list")
    _sign_in(app, seat)
    _wait_connected(app, seat)

    b = app.backups

    # Deadline poll with a page RE-ENTRY each round, exactly like
    # `_adopt_or_create_set` (a fresh mount re-fetches the list). A single-shot
    # read would make "the list has not loaded yet" indistinguishable from "this
    # client cannot see the set" — the negative assertion below is only sound
    # once the client has been given a named, generous budget to produce the row
    # (convention 14: a green run returns on the first poll and pays nothing).
    titles: list[str] = []
    idx = None
    deadline = time.monotonic() + PREFLIGHT_WINDOW
    while True:
        b.navigate_folders()
        app.driver.wait_for("folder-add-button", timeout=30)
        # The add button is the PAGE's readiness signal, not the ROW LIST's —
        # let this mount actually populate before reading it (see the helper).
        _await_folder_rows(b)
        titles = _rendered_folder_titles(b)
        idx = _folder_index(b)
        if idx is not None:
            break
        if time.monotonic() >= deadline:
            break
        print(
            f"[preflight] {seat}: {len(titles)} row(s) so far {titles} — "
            f"{SET_NAME!r} not among them; re-entering the page"
        )
        b.navigate_devices()
        time.sleep(2.0)  # sleep-ok: poll cadence of the deadline loop above, not a settle wait — nothing is asserted about this interval and the loop exits the moment the row is seen

    print(f"[preflight] {seat}: {len(titles)} folder row(s) rendered -> {titles}")
    if idx is None:
        pytest.fail(_diagnose_missing_set_row(app, b, seat, PREFLIGHT_WINDOW))
    print(f"[preflight] {seat}: folders PASS — {SET_NAME!r} is row {idx} ({titles[idx]!r})")

    # ── The MEDIA surface, which the Folders page above does NOT cover ──────
    #
    # Added 2026-08-04 after the macOS leg of run `20260803-04` died at
    # `_await_own_ack_on_nest` while THIS preflight passed. The two reads are
    # different pages backed by different RPCs (`fauna.folders.list` vs
    # `fauna.media.list`) and — the part that cost the round — different
    # client-side RENDER paths: `MediaMachine::render_sealed_paths` omits every
    # row whose label the reader cannot open, so apple's then-keyless
    # `refresh(backupKey: nil)` rendered ZERO media items while the folder row
    # above rendered perfectly. A preflight that stops at the Folders page is
    # blind to the entire surface the round's own-ack barrier actually polls.
    #
    # Same read the round itself uses (`_read_set_listing` — forced navigation
    # edge, lazy-list tolerant), so this cannot drift from the barrier it is
    # de-risking.
    names, rows = _read_set_listing(app)
    print(f"[preflight] {seat}: media listing rendered {rows} row(s), {len(names)} name(s)")
    if rows == 0:
        pytest.fail(
            f"[preflight] {seat}: the Folders page renders {SET_NAME!r} but the MEDIA page "
            f"renders ZERO items. The round's own-ack barrier "
            f"(`_await_own_ack_on_nest`) reads THIS surface, so it would poll a permanently "
            f"empty listing for its whole window and fail this seat.\n"
            f"Known cause of exactly this signature (fixed 2026-08-04; git log --grep 'never passed its owner key'): the "
            f"client passed no owner backup key to `MediaMachine::refresh`, so "
            f"`render_sealed_paths` silently dropped every sealed row — check this client's "
            f"`refresh` call site passes its key before looking anywhere else, and check for a "
            f"`fauna_media` 'media rows omitted' warning in the app log.\n"
            f"Caveat: a genuinely empty {SET_NAME!r} would also read zero here — but the shared "
            f"set is long-lived and carries every prior round's files, so zero means the render, "
            f"not the data."
        )
    print(f"[preflight] {seat}: media PASS — {rows} row(s) visible on the Media page")


@pytest.mark.feature("local-folder-sync")
def test_three_seat_live_sync(app, tmp_path):
    if not RUN_ID:
        pytest.fail(
            "FAUNA_MULTISEAT_RUN_ID is empty and no announce ran in this session. "
            "The seat does not compute the run id — the announce does. Either "
            "collapse both into ONE invocation (one e2e-slot acquisition, "
            "preferred on a loaded box):\n"
            "  pytest tests/test_filesync_multiseat_live.py::test_announce_next_run_id "
            "tests/test_filesync_multiseat_live.py::test_three_seat_live_sync "
            "--client tui -s\n"
            "or run the announce alone, then export the printed id:\n"
            "  pytest tests/test_filesync_multiseat_live.py::test_announce_next_run_id "
            "--client tui -s\n"
            "then `export FAUNA_MULTISEAT_RUN_ID=<printed id>` and re-run this test."
        )
    assert re.fullmatch(r"[A-Za-z0-9_-]{1,40}", RUN_ID), (
        f"run id {RUN_ID!r} must be [A-Za-z0-9_-]{{1,40}} "
        "(it prefixes every run file's basename)"
    )
    if len(SEATS) < 2:
        pytest.fail(
            "need at least 2 seats to rendezvous; "
            f"FAUNA_MULTISEAT_SEATS={os.environ.get('FAUNA_MULTISEAT_SEATS', '')!r} "
            f"resolved to {SEATS}. Set e.g. FAUNA_MULTISEAT_SEATS=linux,macos."
        )
    seat = _seat_of(app.driver)
    if seat not in SEATS:
        pytest.fail(
            f"this client's seat {seat!r} is not in the cohort {SEATS}. Run a "
            "--client whose seat is in FAUNA_MULTISEAT_SEATS, or add it to the cohort."
        )
    peers = [s for s in SEATS if s != seat]
    print(f"[multiseat] seat={seat} cohort={SEATS} creator={CREATOR} "
          f"run={RUN_ID} window={WINDOW:.0f}s")

    _sign_in(app, seat)
    _wait_connected(app, seat)

    # Phase 0 — freshness self-check through the Media UI (before any write).
    _assert_run_id_fresh(app, seat)

    b = app.backups
    _adopt_or_create_set(app, b, seat)

    folder = tmp_path / "multiseat"
    folder.mkdir()
    _bind_folder(app, b, seat, folder)

    try:
        # Phase 1 — create + rendezvous: my hello (and, on linux, the shared
        # base) out; both peers' hellos + the identical base in.
        _write(_hello_path(folder, seat), _hello(seat))
        if seat == CREATOR:
            _write(_shared_path(folder), _shared({s: "base" for s in SEATS}))
        print(
            f"[multiseat] {seat}: phase 1 — wrote hello, awaiting peers "
            f"(assembly budget {ASSEMBLY_WINDOW:.0f}s — a peer may still be in "
            f"its machine's e2e slot queue)"
        )
        expect = {_hello_path(folder, p): _hello(p) for p in peers}
        expect[_shared_path(folder)] = _shared({s: "base" for s in SEATS})
        # The cohort ASSEMBLES here: this is the only wait that spans another
        # machine's slot queue and cold build, so it gets the generous budget.
        _await_files(
            seat,
            app,
            expect,
            "phase 1 (create + rendezvous)",
            budget=ASSEMBLY_WINDOW,
        )

        # Phase 2 — edit barrier: everyone announces readiness, so the edits
        # below start within one sync latency of each other on all seats.
        _write(_ready_path(folder, seat), _ready(seat))
        print(f"[multiseat] {seat}: phase 2 — ready, awaiting peers")
        _await_files(
            seat,
            app,
            {_ready_path(folder, p): _ready(p) for p in peers},
            "phase 2 (edit barrier)",
        )

        # Phase 3 — concurrent edit: rewrite ONLY my line of the shared file
        # (read-modify-write on whatever is current — a peer's edit may already
        # have merged in locally, and preserving it is exactly right), then
        # converge to the byte-exact three-edit merge.
        shared_final = _shared({s: f"edited in run {RUN_ID}" for s in SEATS})
        # POLL the read, don't read once: a peer's concurrent phase-3 edit may be
        # mid-apply locally, and the engine can delete→recreate the file on apply,
        # so a single read races a transient absence (None) or a half-written body
        # (this exact race failed the linux seat on 20260718-03: `_read` -> None).
        # Wait until the file is present AND still carries my base line (a peer's
        # edit already merged in is fine — the replace below preserves it).
        deadline = time.monotonic() + WINDOW
        current = _read(_shared_path(folder))
        while (
            current is None or f"{seat}: base" not in current
        ) and time.monotonic() < deadline:
            time.sleep(0.5)
            current = _read(_shared_path(folder))
        assert current is not None and f"{seat}: base" in current, (
            f"[{seat}] shared file never presented my base line within {WINDOW:.0f}s "
            f"(last read {current!r}) — either a peer's edit-apply keeps the file "
            f"absent/half-written here, or the base never synced to this seat"
        )
        _write(
            _shared_path(folder),
            current.replace(f"{seat}: base", f"{seat}: edited in run {RUN_ID}"),
        )
        print(f"[multiseat] {seat}: phase 3 — edited own line, awaiting merge")
        _await_files(
            seat,
            app,
            {_shared_path(folder): shared_final},
            "phase 3 (concurrent three-way merge convergence)",
        )

        # Phase 4 — distributed convergence ack: prove the OTHER two machines
        # also reached the identical final bytes, not just this one.
        digest = hashlib.sha256(shared_final.encode()).hexdigest()
        _write(_done_path(folder, seat), _done(seat, digest))
        print(f"[multiseat] {seat}: phase 4 — done written, awaiting peer acks")
        _await_files(
            seat,
            app,
            {_done_path(folder, p): _done(p, digest) for p in peers},
            "phase 4 (distributed convergence ack)",
        )
        _await_own_ack_on_nest(app, seat)

        # Phase 5 — cross-machine delete propagation (5a barrier, then 5b
        # unlink + observe). Each seat deletes only its OWN done file, so three
        # tombstones are applied on two peers apiece: one run covers every
        # ordered (recording OS -> applying OS) pair, which is exactly the
        # certification a single machine cannot give. `file-sync.md` § *Applying
        # a remote delete must not record one back* is why it must be cross-OS —
        # the echo-suppression token collision is reachable on macOS's event
        # latency and not on Linux's, so a green Linux run does not absolve a
        # daemon of it. Ordering contract + its rationale: `_run_delete_phase`.
        _run_delete_phase(
            seat,
            folder,
            peers,
            lambda expect: _await_files(seat, app, expect, "phase 5a (delete barrier)"),
            lambda paths: _await_deleted(
                seat, app, paths, "phase 5b (cross-machine delete propagation)"
            ),
        )
        print(f"[multiseat] {seat}: cohort converged ({'+'.join(SEATS)}) — PASS")
    finally:
        _unbind_folder(app, b)

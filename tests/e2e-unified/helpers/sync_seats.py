"""N sync seats on ONE machine — the default shape for a live sync-convergence
test (testing.md § convention 16, ratified 2026-08-01; extended from two seats
to N 2026-08-02).

Why this exists
---------------
The tri-machine round (``tests/test_filesync_multiseat_live.py`` § THE OPERATOR
HANDSHAKE) needs a human at each of three machines for every run, so it cannot
be the workhorse for "many more sync tests". This module is the seam for the
shape that can be: one pytest process runs **N independent seats on one
machine** — same account seed, N per-``sync.db`` device ids (the laptop+tablet
scenario ``libs/fauna-sync-engine/src/config.rs::device_id`` mints ids for), N
isolated watch dirs — against the **live nest**, and asserts convergence
directly. No operator, no cohort, no ``go``-wait.

**Two seats is the default; three is the fan-out certification.** Two seats can
only ever state pairwise facts, so three is what proves a write reaches *every*
peer, a concurrent edit merges identically on all of them, and a delete
propagates to more than one. Everything here is written over a seat *list* so
the count is a caller's choice, never a shape baked into the helpers.

What is NOT ported from the tri-machine round
---------------------------------------------
Its ceremony is coordination for machines that cannot see each other, and one
process has a side channel — its own memory. So there is **no announce and no
``YYYYMMDD-NN`` id**: :func:`new_run_token` mints a local
``2seat-<YYYYMMDD>-<8 hex>`` token, unique by construction, which makes phase-0
freshness a property of the token rather than a nest read. The token namespace
is deliberately **disjoint** from the announce's ``^\\d{8}-\\d{2}-`` prefix
(``multiseat_config._RUN_ID_PREFIX``), so residue from a crashed two-seat run
can never be counted into — and therefore never perturb — the tri-machine id
sequence. ``tests/test_sync_seats.py`` pins that behaviourally, against the
announce's own arithmetic rather than against the regex.

The seat seam
-------------
:class:`SeatDriver` is what a multi-seat test talks to, so the phase I/O
(write / read / deadline-poll) stays seat-agnostic and the driver axis —
tui, native — is additive. :class:`AppSeatDriver` drives a real app, whose
resident ``fauna-sync-agent`` signs every change record it writes.

**The disk-only seat is retired (2026-09-30).** It was a headless
``fauna-sync`` daemon, and the nest refuses every change record that carries
no writer signature: the daemon's data plane carries none
(``mls-group-key-material.md`` § Implementation status today, the
headless-daemon residual). A seat that cannot record cannot converge,
and the agent — the only resident app-free engine — signs with the principal
writer key only an app's enrollment ceremony certifies
(``bins/fauna-sync-agent/src/engine_driver.rs``,
``principal_bundle::load_change_signer``). So the lightest RESIDENT seat is now
``tui``. (A one-shot signed WRITER needs no app: ``helpers.harness_writer``
builds the shared engine for one pass with the identity seed, which signs
directly — the writer a test with a daemon READER uses. It never pulls, so it
is not a seat.)

:func:`make_seat` is the single
construction point both environments (harness nest and live box) and both seat
counts share — four call sites would otherwise each grow their own copy and
drift apart on the axis that matters least and breaks most (priority #4).

One member departs from the design sketch's ``applied_delete_paths()``:
:meth:`SeatDriver.observed_delete` asks the question per-basename instead of
returning a set, because a UI seat can only witness absence-in-folder for a name
it is asked about — the agent logs paths **redacted** (``path~<hex>``,
``fauna_core::log_redact::log_path``), so no comparable set of basenames exists
to return.

Stdlib-only apart from the sibling helpers and the generated ``i18n.strings``
(itself pure data, no driver and no process), so every pure half is pinned by
tier_1 tests with no process and no nest in sight.
"""

from __future__ import annotations

import contextlib
import os
import re
import secrets
import sys
import time
from pathlib import Path
from typing import Protocol, runtime_checkable

from helpers import multiseat_config as cfg
from helpers.waiting import wait_until
from i18n.strings import S

# The seats are named for their ROLE. `linux`/`macos`/`windows` identify
# MACHINES in the tri-machine round (`multiseat_config._PLATFORM_SEATS`), and
# seats on one machine are not machines — reusing those names would make a
# failure message claim a cross-machine fact this test cannot hold.
#
# The pool is ordered and a run takes a prefix of it (:func:`seat_names`), so
# seat `a` means the same thing — the creator/deleter — in a two- and a
# three-seat run alike.
SEAT_NAMES = ("a", "b", "c")


def seat_names(count: int) -> tuple:
    """The first ``count`` seat names.

    Raises rather than truncating silently past the pool: a run that asked for
    more seats than there are names would otherwise collide two seats onto one
    name, and the legs key every observation on the name.
    """
    if not 2 <= count <= len(SEAT_NAMES):
        raise ValueError(
            f"a one-machine run takes 2..{len(SEAT_NAMES)} seats, not {count} "
            f"(add names to SEAT_NAMES to raise the ceiling)"
        )
    return SEAT_NAMES[:count]

# How a seat is driven. `tui` and `native` drive the real UI for sign-in /
# adopt / bind and then leave the subject to plain file I/O. The retired
# `engine` (headless-daemon) mode is gone for good — see the module docstring.
SEAT_MODES = ("tui", "native")

# The tui UI pair and the native-app pair. The mixed pair (`tui+native`) comes
# free from the axis as the cheapest bisect there is: green `tui+tui` beside a
# red `native+native` already localises the fault to the native app stack, and
# `native+tui` narrows it to one seat. It is not collected by default only
# because it costs two app launches; add it to a run through
# `$FAUNA_SEAT_SETS`.
DEFAULT_PAIRS = (("tui", "tui"), ("native", "native"))

# The three-seat matrix: the same modes, one seat wider. Same shape rather than a
# hand-picked subset, so a platform's three-seat coverage never quietly differs
# from its two-seat coverage in a way nobody declared (priority #1) — the
# platform filter below is what removes what a box genuinely cannot drive.
DEFAULT_TRIOS = tuple((mode,) * 3 for mode in SEAT_MODES)

# The DEFAULT MATRIX the one folded module collects — ruling 1 of the seat-module
# fold (2026-08-03, testing.md § convention 16): every mode-uniform set at both
# ratified seat counts, and nothing else. Seat count is COVERAGE here, not a
# multiplier to schedule away — both counts stay inner-loop default, exactly the
# union the two per-count modules collected before the fold. The expensive
# multipliers stay where the axis put them: nest modes are scheduled sweeps
# (Mode mechanics: never inner-loop multipliers) and the mixed diagnostic sets
# are env-opt-in (`$FAUNA_SEAT_SETS`; each app seat costs a full app launch).
DEFAULT_SEAT_SETS = DEFAULT_PAIRS + DEFAULT_TRIOS

# Seat modes this OS CANNOT DRIVE, and why — a STRUCTURAL absence, closed
# forever, never unbuilt debt (convention 7). tui on windows drives no sync agent
# at all (`seat_app_error` refuses the pairing; the same ruling
# `test_filesync_delete_declined_debounce.py` cites), so a `tui` seat there could
# never sync a byte — the pair is filtered out of collection rather than
# collected and skipped, because a pair that cannot exist is not coverage this
# platform is missing.
UNSUPPORTED_MODES = {"win32": ("tui",)}

# Seat modes whose driver is BUILT on some platforms and STILL OWED on others —
# debt with a named owner. Deliberately a separate map from
# :data:`UNSUPPORTED_MODES`, because convention 7's whole point is that "cannot
# exist" and "nobody has built it yet" must never wear the same word: filing the
# macos seat as *unsupported* would teach every future reader it is impossible,
# which is false, and is how an owed track quietly dies. This map shrinks toward
# `{}` as each platform's seat lands; that one is closed forever.
#
# EMPTY since 2026-08-10 — the shape it was always aimed at. windows (2026-08-02),
# macos (2026-08-06) and linux (2026-08-10) all build their `native` arm, so every
# platform now collects every set the matrix declares and `unbuilt_note` is silent
# everywhere. Keep the map (and its printer) rather than deleting them: the next
# seat mode to be built somewhere-but-not-everywhere files its debt here, and the
# reason strings are the only place an owner gets named. Two of the three landings
# found a real product defect on their first run, so an entry here is a *pending
# bug report*, not bookkeeping.
#
# `native` means "this platform's own GUI app seat", resolved to a concrete
# app by :data:`NATIVE_APPS`, which `make_seat` is the sole consumer of.
UNBUILT_MODES: dict[str, dict[str, str]] = {}


def seat_sets_for_platform(sys_platform: str, seat_sets=None) -> tuple:
    """The seat sets collectable on this OS — the given matrix (default
    :data:`DEFAULT_PAIRS`) minus any set naming a mode this platform cannot
    drive *or* has not built yet.

    Length-agnostic on purpose: the same filter serves the two-seat matrix, the
    three-seat one, and any explicit set from :func:`seat_sets_from_env`. A
    per-count copy would be one more place for a platform's exclusions to fall
    out of step.

    Filtering at COLLECTION rather than skipping inside the test is deliberate.
    A skip reports `s` in a summary line that reads like success (convention 7).
    The two exclusion reasons are kept apart in the maps above so the *reason*
    survives even though the mechanism is the same; :func:`unbuilt_note` is what
    keeps the debt half visible. The native set collects everywhere, so no
    platform is left with nothing.
    """
    absent = tuple(UNSUPPORTED_MODES.get(sys_platform, ())) + tuple(
        UNBUILT_MODES.get(sys_platform, {})
    )
    return tuple(
        seat_set
        for seat_set in (DEFAULT_PAIRS if seat_sets is None else seat_sets)
        if not any(mode in absent for mode in seat_set)
    )


SEAT_SETS_ENV = "FAUNA_SEAT_SETS"


def seat_sets_from_env(environ=None) -> tuple | None:
    """An explicit seat-set list from ``$FAUNA_SEAT_SETS``, or None for the
    default matrix.

    The MIXED set is the designed instrument for localizing a multi-seat red:
    `native+tui` answers "does the native seat upload?" separately from
    "does the native seat receive?", because the tui half is already proven by
    the `tui+tui` set in the same run. The axis has
    always admitted mixed sets (:func:`seat_sets_for_platform` takes an
    override), but they are not collected by default — each app seat costs a
    full app launch — so before this hook the only way to run one was to edit
    the parametrization. That is a change a session then has to remember not to
    commit, which is how a diagnostic stops being reached for.

    Format: comma-separated `+`-joined modes of two or more, e.g.
    ``native+tui`` or ``tui+tui+native,native+tui``. The set's
    LENGTH is the seat count, so this is also how a three-seat bisect is
    invoked. Unknown modes raise rather than silently collecting nothing.
    """
    raw = (os.environ if environ is None else environ).get(SEAT_SETS_ENV, "").strip()
    if not raw:
        return None
    seat_sets = []
    for chunk in raw.split(","):
        chunk = chunk.strip()
        if not chunk:
            continue
        modes = tuple(m.strip() for m in chunk.split("+"))
        if not 2 <= len(modes) <= len(SEAT_NAMES) or not set(modes) <= set(SEAT_MODES):
            raise ValueError(
                f"{SEAT_SETS_ENV}: {chunk!r} is not 2..{len(SEAT_NAMES)} modes "
                f"from {SEAT_MODES}"
            )
        seat_sets.append(modes)
    return tuple(seat_sets)


def unbuilt_note(sys_platform: str) -> str | None:
    """What this platform is NOT collecting because nobody built it yet — or
    None when it owes nothing.

    No silent caps: a platform that collects fewer seat sets than the matrix
    declares has to say so, or a green run reads as full coverage. Structural
    absences are deliberately NOT reported here — they are not coverage this
    platform is missing, and repeating them every run would train readers to
    ignore the line that does matter.
    """
    owed = UNBUILT_MODES.get(sys_platform, {})
    if not owed:
        return None
    return "\n".join(
        [f"[seats] seat modes not collected on {sys_platform} — UNBUILT, not impossible:"]
        + [f"  {mode}: {reason}" for mode, reason in sorted(owed.items())]
    )


# The token's leading digit is the SEAT COUNT, so residue on the shared live set
# says which run shape left it without anyone having to cross-reference a log.
RUN_TOKEN_RE = re.compile(r"\d+seat-\d{8}-[0-9a-f]{8}")


def token_prefix(seats: int) -> str:
    """``2seat-``, ``3seat-`` — the basename prefix for an N-seat run."""
    return f"{seats}seat-"


def new_run_token(
    *, seats: int = 2, today: str | None = None, rand_hex: str | None = None
) -> str:
    """Mint this run's token: ``<N>seat-<YYYYMMDD>-<8 lowercase hex>``.

    Freshness is BY CONSTRUCTION — 32 bits of ``secrets`` entropy per run, so no
    file carrying this token can already exist on the shared set. That is the
    whole reason the one-machine test needs no announce, no reader app and no
    phase-0 freshness read: the tri-machine round derives its id from the set
    precisely because three machines must agree with no side channel, and one
    process has one.

    Every seat count shares one namespace shape, and every member of it stays
    disjoint from the announce's ``^\\d{8}-\\d{2}-`` prefix — a leading
    ``<digits>seat-`` can never be read as a ``YYYYMMDD-NN`` run file, so
    residue from a crashed run of ANY size can never perturb the tri-machine id
    sequence. ``tests/test_sync_seats.py`` pins that behaviourally, against the
    announce's own arithmetic rather than against the regex.

    The date is carried purely so a human reading residue on the live set can
    tell when it was left. ``today``/``rand_hex`` are injectable for the tier_1
    pins.
    """
    token = (
        f"{token_prefix(seats)}{today or cfg.today_str()}-"
        f"{rand_hex or secrets.token_hex(4)}"
    )
    if not RUN_TOKEN_RE.fullmatch(token):
        raise ValueError(
            f"minted run token {token!r} is malformed — it prefixes every run "
            f"file's basename and must match {RUN_TOKEN_RE.pattern}"
        )
    return token


def seat_set_id(seat_set) -> str:
    """A seat set's bare name: ``tui+tui``, ``tui+tui+tui``,
    ``native+tui``. Used in log lines where the count is already stated;
    parametrization ids use :func:`seat_set_param_id`."""
    return "+".join(seat_set)


def seat_set_param_id(seat_set) -> str:
    """The parametrization id for a seat set: ``2seat-tui+tui``,
    ``3seat-tui+tui+tui`` — ruling 4 of the seat-module fold (2026-08-03).

    Leads with :func:`token_prefix`, the same prefix the run token and
    therefore any live-set residue carries, so a flake-history id, a
    ``-k 2seat``/``-k 3seat`` selection and a leftover file on the shared set
    all speak one vocabulary. The count is redundant with the set's length on
    purpose: without it, ``-k tui+tui`` substring-matches the trio id
    too, and the pairs would be unselectable — the per-count selection the
    retired per-count files used to provide has to live somewhere.
    """
    return f"{token_prefix(len(seat_set))}{seat_set_id(seat_set)}"


def residue_note(token: str, seats) -> str | None:
    """Every file this run created and did not delete, named seat by seat — or
    None when there is none.

    The shared-box rule's non-destructive carve-out (testing.md § shared-box
    rule) is the *only* thing licensing an unattended run against example.com, and
    it is conditional on the run removing what it created. So a failed cleanup
    is not hygiene, it is the licence lapsing: the message has to name the files
    precisely enough that a human can finish the job from the log alone.

    ``seats`` is an iterable of ``(name, folder)``. Only ``<token>-*`` is ever
    reported: another run's files are neither this run's to report nor its to
    delete.
    """
    left = []
    for name, folder in seats:
        try:
            names = sorted(p.name for p in Path(folder).glob(f"{token}-*"))
        except OSError:
            names = []
        if names:
            left.append(f"  seat {name} ({folder}): {', '.join(names)}")
    if not left:
        return None
    return (
        f"LIVE-SET RESIDUE — run {token} left files behind on the shared set. "
        f"An unattended run against the live box is licensed only by the "
        f"non-destructive carve-out, which requires this run to delete "
        f"everything it created. Delete them from any app (they are invisible "
        f"to the tri-machine announce, so they harm nothing meanwhile), or re-run "
        f"this test — it deletes every {token}-* it finds.\n" + "\n".join(left)
    )


def finalize_live_residue(run_token: str, seats, budget: float = 60.0) -> None:
    """Best-effort removal of anything a live run left behind, then a loud warning.

    Runs while the seats are still ALIVE — a deleted file only leaves the nest if
    a running daemon notifies it — and never raises: it executes in a ``finally``
    that may already be unwinding the body's own failure, and masking that
    failure would be worse than leaking a file.

    Every peer is waited on, not just one. With two seats "the peer" was
    unambiguous; with three, confirming one peer and calling it done would let a
    tombstone that reached only half the account report a clean finalize — the
    same narrowing the legs themselves are written to avoid.
    """
    try:
        held = {
            seat.name: sorted(p.name for p in seat.path.glob(f"{run_token}-*"))
            for seat in seats
        }
        if not any(held.values()):
            return  # the green path: leg 5 already cleaned up and proved it did

        # Delete from ONE seat and let the deletion propagate, rather than
        # unlinking in every folder: a seat that deleted its own copy locally
        # never applies the peer's tombstone, so unlinking everywhere would
        # destroy the only evidence that the files left the NEST — which is the
        # half the carve-out is actually about.
        deleter = next(s for s in seats if held[s.name])
        watchers = [s for s in seats if s is not deleter]
        names = held[deleter.name]
        for name in names:
            with contextlib.suppress(OSError):
                (deleter.path / name).unlink()
        print(
            f"[seats] finalizer: the body ended before the cleanup leg — "
            f"deleted {', '.join(names)} on seat {deleter.name}, waiting up to "
            f"{budget:.0f}s for {', '.join(w.name for w in watchers)} to apply "
            f"the deletions",
            flush=True,
        )
        # (watcher name, basename) pairs, so a partially-propagated tombstone is
        # reported as exactly the seats that never saw it.
        unconfirmed = [(w, n) for w in watchers for n in names]

        def _confirmed() -> bool:
            unconfirmed[:] = [
                (w, n) for w, n in unconfirmed if not w.observed_delete(n)
            ]
            return not unconfirmed

        # Best-effort: a timeout here is a warning, never a failure, so the
        # shared waiter's AssertionError is swallowed rather than raised.
        with contextlib.suppress(AssertionError):
            wait_until(_confirmed, budget, interval=1.0)
        if unconfirmed:
            missed = sorted({f"{w.name}:{n}" for w, n in unconfirmed})
            print(
                f"⚠️  [seats] these seats never applied the delete of these "
                f"files: {', '.join(missed)} — they are PROBABLY still on the "
                f"live set. They are invisible to the tri-machine announce (the "
                f"<N>seat- token namespace is disjoint), so they harm nothing "
                f"meanwhile; delete them from any app when convenient.",
                flush=True,
            )
        # Anything still on disk anywhere is removed unconditionally now — the
        # local half of the carve-out, independent of whether the nest half
        # could be confirmed.
        for seat in seats:
            for p in seat.path.glob(f"{run_token}-*"):
                with contextlib.suppress(OSError):
                    p.unlink()
        note = residue_note(run_token, [(s.name, s.path) for s in seats])
        if note:
            print(note, flush=True)
    except Exception as exc:  # never mask the body's own failure
        print(f"⚠️  [seats] finalizer itself failed: {exc!r}", flush=True)


# Which concrete app a `native` seat means on each platform. One mode name, one
# resolution point — so `seat_set_id` reads `native+native` on every box and a
# new platform's seat is a row here rather than a new mode name (#1/#3).
#
# All three GUI desktops as of 2026-08-10. `linux` is the one row whose platform
# key and app name coincide (`sys.platform == "linux"`, the app is `linux`);
# that is a coincidence of naming, not a rule — do not "simplify" the lookup into
# an identity fallback, which would silently resolve a `native` seat on any future
# unix that has no app at all, exactly the case `make_seat`'s NotImplementedError
# exists to make loud.
NATIVE_APPS = {"win32": "windows", "darwin": "macos", "linux": "linux"}


def native_app_for_platform(sys_platform: str) -> str | None:
    """The app a ``native`` seat drives here, or None if this platform has none."""
    return NATIVE_APPS.get(sys_platform)


def windows_agent_data_dir(root) -> Path:
    """Where a windows seat's own sync agent keeps its state — and its log.

    ONE owner for this path. :func:`isolate_windows_sync_agent` pins it into the
    launch environment and :class:`AppSeatDriver` reads the agent's rolling log
    back out of it at failure time; two literals would drift apart silently and
    the second reader would just report "no log" forever.
    """
    return Path(root) / "sync-agent"


# `fauna_log::init` (bins/fauna-sync-agent/src/lib.rs) installs a DAILY-rolling
# file writer at `<data_dir>/logs/fauna.log.<date>` alongside stderr. The file is
# the only channel that survives here: the windows app spawns the agent with
# `UseShellExecute=false` and no redirection (`App.xaml.cs::SpawnSyncAgentDetached`),
# so its stderr goes to whatever the app inherited and never reaches pytest.
AGENT_LOG_SUBDIR = "logs"
AGENT_LOG_GLOB = "fauna.log.*"


def isolate_windows_sync_agent(config: dict, *, root, seat: str, agent_bin: str) -> dict:
    r"""Give ONE windows seat its own sync agent — pipe, state and binary.

    Windows is the extreme case of convention 10. The unix apps' agent socket
    is path-derived, so a driver's private HOME/XDG world isolates it for free;
    windows rendezvouses on the machine-global ``\\.\pipe\fauna-sync.<SID>`` and
    defaults its state to ``%LOCALAPPDATA%``, so both must be named explicitly or
    the seat drives — and provisions — the DEVELOPER's own installed agent. That
    is the exact 2026-07-24 multiseat failure.

    **Set directly rather than through the ``real_sync_agent`` /
    ``isolated_sync_agent`` markers, which is a deliberate departure from every
    other windows agent test.** Those markers resolve through SESSION-scoped
    fixtures (``conftest.isolated_sync_agent_pipe_name`` /
    ``isolated_sync_agent_data_dir``) that yield ONE value per session — correct
    while the windows app is session-scoped (``_driver_cache``), and precisely
    wrong here. Two seats sharing one pipe name is the worst available failure:
    seat b's app adopts seat a's already-serving agent, both folders bind to one
    engine, and the pair looks healthy while proving nothing about device-to-
    device sync. A per-seat value cannot come from a session-scoped fixture, so
    each seat names its own.

    The agent's credential dir is deliberately NOT set here: the windows driver
    overwrites ``FAUNA_E2E_CREDENTIAL_DIR`` with its own per-instance mkdtemp at
    launch (``drivers/windows.py``), and each seat builds its own driver, so the
    seats are already isolated on that axis. Setting it would be dead code that
    reads as load-bearing.
    """
    env = config.setdefault("environment", {})
    # Gates whether the app's hydration/provisioning loop runs at all.
    env["FAUNA_E2E_REAL_SYNC_AGENT"] = "1"
    # One pipe PER SEAT — keyed on the seat letter, since both seats live in
    # this one pytest process and a pid alone would collide them.
    env["FAUNA_E2E_SYNC_PIPE"] = f"fauna-sync-e2e-seats-{os.getpid()}-{seat}"
    agent_dir = windows_agent_data_dir(root)
    agent_dir.mkdir(parents=True, exist_ok=True)
    env["FAUNA_E2E_SYNC_AGENT_DATA_DIR"] = str(agent_dir)
    # Without this pin a dev tree stages no agent beside the app and the spawn
    # silently does nothing (conftest._apply_isolated_sync_agent_env).
    env["FAUNA_E2E_SYNC_AGENT_BIN"] = agent_bin
    return config


def pin_macos_seat_sync_agent(config: dict, *, agent_bin: str) -> dict:
    """Give ONE macos seat a real sync agent of its own — flag plus binary pin.

    Two env vars, and the *absence* of a third is the whole story. macOS is the
    easy case of convention 10 where windows is the hard one: the agent socket is
    ``~/Library/Application Support/Fauna/sync-agent.sock``, derived from
    ``dirs::home_dir()`` (`fauna-ipc/src/unix_transport.rs::macos_socket_path`),
    and the macos driver already relocates ``HOME``/``CFFIXED_USER_HOME`` to a
    private per-launch tree (`drivers/macos.py::launch`). So each seat's app AND
    its child agent land on a distinct socket for free — there is no machine-
    global rendezvous point to name per seat, which is exactly what
    :func:`isolate_windows_sync_agent` has to do for the per-SID pipe.

    **Set directly rather than through the ``real_sync_agent`` marker, for a
    different reason than windows'.** The marker resolves session-wide
    (`conftest._apply_real_sync_agent_env` reads *every* collected item), so
    marking this module would arm a real child agent for every macos launch in
    any sweep that collects it — including tests that assert today's no-agent
    behaviour. The seat needs the agent by construction, not by what else the
    session happened to select, so it names it here. The session-scoped
    ``macos_sync_agent_binary`` fixture the caller resolves is still the right
    shape to share: it is a build artifact, not a rendezvous name.

    Without the binary pin the spawn would silently resolve against PATH and
    start the box's INSTALLED ``/usr/local/bin/fauna-sync-agent`` — stale,
    machine-global, and shared with the developer's own session (the app is
    built by SwiftPM under ``.build/`` while the agent is a cargo binary under
    ``target/debug/``, so the shared sibling-of-exe probe cannot hit).
    """
    env = config.setdefault("environment", {})
    # Gates whether the app provisions an agent at all — on macOS the arm that
    # reads it is the A4 live-e2e carve-out in `FaunaMacApp`'s session-patch
    # login (`FaunaE2E.realSyncAgent`), since an e2e login never reaches
    # `completeAuthenticatedLaunch` where the production spawner is built.
    env["FAUNA_E2E_REAL_SYNC_AGENT"] = "1"
    env["FAUNA_E2E_SYNC_AGENT_BIN"] = agent_bin
    return config


# The badges a conflict review row may carry once it has AUTO-RESOLVED — the two
# resolution arms of `fauna_folders_machine::conflict_badge_label`, which is the
# one owner of that map for all 7 apps.
#
# Read off the generated i18n module rather than hard-typed here. The strings are
# `devices.conflicts.resolved_*` in `i18n/strings/en.yaml`, so a copy edit there
# regenerates this set instead of silently turning the assertion below into a
# tautology — the failure mode that makes a badge assertion worthless.
#
# Deliberately an ALLOWLIST, not a denylist of unresolved spellings. An
# unresolved row renders whatever `conflict_badge_label` falls through to: the
# localized type for the two known types, and for the engine's live
# `concurrent_edit` — which has no i18n key — the RAW WIRE STRING. Enumerating
# those is a losing game, and a new spelling must fail RED, not slip through.
RESOLVED_BADGES = frozenset(
    {
        S.devices.conflicts.resolved_merged,
        S.devices.conflicts.resolved_latest_wins,
    }
)


# How many times a UI seat re-reads the review list looking for a row count
# that holds still across the read (see `AppSeatDriver.review_rows`).
_REVIEW_READ_ATTEMPTS = 3

# How long a UI seat's navigation to the Folders page may take before the
# review read gives up. Sized for a loaded box, not an idle one: this is a real
# app's page swap competing with other builds on the same machine, and the
# driver's 10 s default is a wall-clock guess that a busy machine loses
# (convention 14 — a green nav returns on the first poll and pays none of this).
_REVIEW_NAV_BUDGET_S = 90.0

# Lines of the app seat's agent log carried into a failure message. Matches the
# engine seat's `log_tail(n=40)` — the two seats' diagnostics sit side by side in
# one failure, so an asymmetric depth just makes them harder to compare.
_AGENT_LOG_TAIL_LINES = 40


def _is_pipe_poll_line(line: str) -> bool:
    """A DEBUG line for one request on the agent's own pipe — the app's status
    polling, never the sync path (see :meth:`agent_log_note`)."""
    return " DEBUG " in line and "fauna_sync_agent::pipe_server: pipe request" in line


_RESCAN_ARMED = re.compile(r"\brescan_interval_ms=(\d+)")


def adopted_rescan_ms(lines) -> set[int]:
    """Every rescan cadence an agent log says a tick was ARMED with, in ms.

    The engine logs ``rescan_interval_ms=<n>`` once per folder when a resident
    host arms its tick (``fauna_sync_engine::always_resident::log_rescan_armed``)
    — the only observable of the cadence a seat adopted, since the tick itself
    is silent. A set, because a seat with several folders arms several ticks
    and every one of them must agree with the launch.
    """
    return {int(m.group(1)) for line in lines for m in _RESCAN_ARMED.finditer(line)}


def review_rows_for_run(rows, run_token: str) -> list:
    """The ``(badge, file_info)`` rows naming this run's token.

    A conflict review list is per *set*, and the shared live set carries rows
    from every earlier run (they are resolved history — nothing destroys them).
    Filtering on the token is what keeps this run's assertion about this run:
    every file it creates is ``<token>-*``, and `file_info` is the
    display-ready ``"{set}: {path} → {head}"`` line, so the token appears in it
    verbatim.
    """
    return [(badge, info) for badge, info in rows if run_token in info]


def unresolved_review_rows(rows, run_token: str) -> list:
    """This run's review rows that did NOT auto-resolve.

    The whole point of the assertion this feeds: the engine's auto-resolve is
    **fail-closed** (`fauna-sync-engine/src/engine.rs::auto_resolve_conflict`) —
    if the local version's upload or the resolved report fails, it degrades to a
    local unresolved row plus a legacy unresolved report, and the file still
    converges. So the bytes alone cannot tell a clean three-way merge from a
    degraded one; the review row is the only place that difference is visible,
    and it is visible to the USER, which is what makes it worth asserting.
    """
    return [
        (badge, info)
        for badge, info in review_rows_for_run(rows, run_token)
        if badge not in RESOLVED_BADGES
    ]


def make_seat(
    mode: str,
    *,
    name: str,
    run_token: str,
    root: Path,
    node_url: str,
    node_port: int | None,
    folder: str,
    sign_in,
    request,
    rescan_secs: int | None = None,
):
    """One seat, driven by ``mode`` — the single construction point.

    Every isolation axis is per-seat: an app seat gets its own ``sync.db``,
    chunk cache and merge bases from the driver's per-launch private HOME/XDG
    world (convention 10), which also makes its agent socket per-seat; windows
    is the one platform that needs :func:`isolate_windows_sync_agent` on top.

    What the CALLER supplies is exactly what differs between environments:
    ``node_url``/``folder`` and ``sign_in``, which carries the account seed.
    The live twins sign in the way a user does (seed import + the REAL DoH
    handle check, ``test_filesync_multiseat_live._sign_in``) because the handle
    is real there and onboarding is production code worth exercising
    (convention 8); a harness nest has no handle in DNS at all, so its callers
    pass the fixture-setup ``set_state`` carve-out instead. Same seam, same
    legs, two environments — and now one constructor, so a fix to either
    reaches every seat count.

    ``node_port`` is where the harness reads that nest's identity, so an app
    seat launches with its escrow trust seeded for the nest it syncs against
    (`conftest._apply_r14_trust_env` returns early on a handle with no port,
    which left every app seat's generation plane dormant). None is only for a
    nest the harness cannot dial on a local port.

    ``rescan_secs`` is the reconcile backstop's cadence for this seat — the
    engine's compile-gated ``FAUNA_E2E_RESCAN_MS`` seam, reached through the
    app launch's ``config["environment"]`` (the app's resident agent inherits
    it). Since phase 5 of the folders re-model made the production cadence a
    constant, this seam is the ONLY way a run chooses it; ``None`` leaves the
    seat on the drivers' 60 s harness default.
    """
    if mode in ("tui", "native"):
        from conftest import _build_app_config

        if mode == "native":
            app_name = native_app_for_platform(sys.platform)
            if app_name is None:
                # Loud, never a skip: `seat_sets_for_platform` should already
                # have filtered this set out of collection, so reaching here
                # means the two disagree — a harness bug, not missing coverage.
                raise NotImplementedError(
                    f"no native app seat on {sys.platform!r}, yet a `native` "
                    f"seat set was collected — sync_seats.NATIVE_APPS and "
                    f"seat_sets_for_platform disagree."
                )
        else:
            app_name = "tui"

        config = _build_app_config(app_name, {"url": node_url, "port": node_port}, request)
        if rescan_secs is not None:
            config.setdefault("environment", {})["FAUNA_E2E_RESCAN_MS"] = str(
                int(rescan_secs) * 1000
            )
        agent_data_dir = None
        if app_name == "windows":
            # Windows is the one platform whose agent is NOT isolated by the
            # driver's private world — see the helper for why this bypasses the
            # `real_sync_agent`/`isolated_sync_agent` markers.
            isolate_windows_sync_agent(
                config,
                root=root,
                seat=name,
                agent_bin=request.getfixturevalue("sync_agent_binary"),
            )
            # Same pinned dir, read back at failure time: it is where the agent's
            # rolling log lands, and that log is the only account of this seat's
            # RECEIVE path (testing.md § convention 16).
            agent_data_dir = windows_agent_data_dir(root)
        elif app_name == "macos":
            # macOS needs the flag and the binary, but no per-seat naming — the
            # socket rides the driver's private HOME. See the helper for why this
            # bypasses the `real_sync_agent` marker too, and for what a missing
            # pin would silently start instead.
            pin_macos_seat_sync_agent(
                config,
                agent_bin=request.getfixturevalue("macos_sync_agent_binary"),
            )
        # linux needs NOTHING here, and the absence is load-bearing enough to
        # state (verified 2026-08-10 against the app, not assumed from tui).
        # Three things each other desktop had to buy, linux gets from convention
        # 10 already: the GTK app's `SystemdAgentSpawner` short-circuits to a
        # direct child spawn under e2e (`sync_agent.rs`; the gate is
        # `FAUNA_E2E_AGENT_PORT`, which `drivers/linux.py` always sets), so no
        # machine-global systemd unit is written; the agent socket is
        # `$XDG_RUNTIME_DIR/fauna/sync-agent.sock` (`fauna-ipc`
        # `unix_transport::linux_socket_path`) and the driver mints a private
        # 0700 runtime dir per launch, so the child inherits a per-seat socket
        # by construction — no windows-style per-seat NAMING; and the agent
        # binary resolves as a sibling of `current_exe`
        # (`agent_spawner::agent_binary`), which hits because `just linux-debug`
        # builds `-p fauna-linux -p fauna-sync-agent` into one `target/debug/`
        # — no macos-style BIN pin, which macOS needs only because SwiftPM puts
        # its app under `.build/`.
        return AppSeatDriver(
            app_name,
            name=name,
            run_token=run_token,
            path=root / "watch",
            launch_config=config,
            sign_in=sign_in,
            folder=folder,
            node_url=node_url,
            agent_data_dir=agent_data_dir,
            expect_rescan_ms=None if rescan_secs is None else int(rescan_secs) * 1000,
        )

    # Loud, never a skip (convention 7): a seat that silently does not run looks
    # exactly like a seat whose sync is broken.
    raise NotImplementedError(
        f"seat mode {mode!r} has no construction yet "
        f"(testing.md § convention 16)."
    )


def start_seats(
    stack,
    modes,
    *,
    make_one,
    startup_window: float,
) -> list:
    """Bring every seat up in sequence and return them, ready to converge.

    **Sequenced, not concurrent**: seat A is fully ready before B is started.
    Cheap, and it keeps each seat's catch-up log unambiguous — the
    creator/adopter asymmetry the tri-machine round has to reason about
    dissolves under sequencing. An app seat's "this device was handed the set"
    check is the adopt step inside ``start`` (the set's row renders in the
    Folders page), so a never-handed set fails there, never as a convergence
    failure.

    The pin that the reconcile cadence set through ``FAUNA_E2E_RESCAN_MS`` is
    the one a seat ADOPTED rides each seat's own ``await_ready`` (the agent logs
    the armed value — :func:`adopted_rescan_ms`), so it holds for every caller
    that passes :func:`make_seat` a ``rescan_secs``.

    ``make_one(mode, name)`` is the caller's closure over its own environment;
    :func:`make_seat` is what it normally calls.
    """
    seats = []
    for name, mode in zip(seat_names(len(modes)), modes):
        seat = make_one(mode, name)
        stack.enter_context(seat)
        print(seat.describe(), flush=True)
        waited = seat.await_ready(startup_window)
        print(
            f"[seats] {name}: ready (+{waited:.0f}s), "
            f"{seat.app_name} app engine serving",
            flush=True,
        )
        seats.append(seat)
    return seats


@runtime_checkable
class SeatDriver(Protocol):
    """One seat of a multi-seat run, however it is driven.

    The test body writes and reads plain files in :attr:`path` and never asks
    how the seat got there, which is what keeps the driver axis additive.
    """

    name: str
    path: Path

    def start(self) -> None:
        """Bring the seat up: signed in, bound to :attr:`path`, syncing."""

    def await_ready(self, budget: float) -> float:
        """Poll until the seat is connected and caught up; seconds waited.

        A green start returns on the first poll and pays none of the budget
        (convention 14); the budget exists only to make the failure sound.
        """

    def self_note(self) -> str:
        """"Am I the broken seat?", answered from evidence THIS seat holds.

        Never a guess about the peer — the rider at testing.md § point 6: a
        failure names the direction that broke, from evidence the failing party
        actually has.
        """

    def observed_delete(self, basename: str) -> bool:
        """Has this seat APPLIED the peer's delete of ``basename``?

        A positive observation, not an absence — it is the causal barrier the
        cleanup leg's negative assertion hangs off (convention 14).
        """

    def review_rows(self) -> list | None:
        """This seat's conflict review list as ``(badge, file_info)`` pairs.

        ``None`` — never ``[]`` — when the seat has no UI to read it from. The
        two answers mean opposite things ("I looked and the list is empty" vs
        "I cannot look"), and collapsing them into ``[]`` would let a seat that
        can never see a conflict report the same clean bill of health as one
        that genuinely saw none (convention 7's whole complaint about silent
        skips, one layer down).
        """

    def diagnostics(self) -> str:
        """Anything else worth having at failure time — log paths, liveness."""

    def stop(self) -> None:
        """Tear the seat down."""


class AppSeatDriver:
    """A seat driven by the REAL app — the same six-member seam, a UI underneath.

    What differs between its two modes (``tui``, ``native``) is only *which app*
    gets the seat syncing: a driver launch, the shared cross-app UI steps (sign
    in, adopt the set, bind the folder), and then the subject is plain file I/O
    in :attr:`folder`. That is the whole point of the seam — the legs
    (``helpers/convergence_legs``) never learn which kind of seat they are
    driving.

    **The app really does sync.** On unix the app spawns the real
    ``fauna-sync-agent`` binary as a direct child into this launch's isolated
    ``XDG_RUNTIME_DIR`` (``apps/fauna-tui/src/sync_agent.rs::platform_spawner``
    → ``fauna_client_sync::agent_spawner::SystemdUserUnitSpawner`` with its e2e
    arm, gated on ``FAUNA_E2E_AGENT_PORT`` which every driver launch sets), so no
    machine-global systemd unit is written and two seats on one box share
    nothing. `just tui-debug` builds ``fauna-tui`` and ``fauna-sync-agent`` side
    by side, which is what makes the spawner's sibling probe hit without a pin.

    **Isolation is the driver's, not this class's** (convention 10): each launch
    already gets a private HOME/XDG world, its own agent port, its own keyring
    and credential store. Two seats therefore need no extra isolation argument —
    the thing that must NOT be shared (the agent socket) is path-derived from a
    world the driver already made private.

    ``sign_in`` is the one genuinely environment-shaped step, so it is injected
    rather than branched on here: the live twin signs in through the real
    onboarding flow (seed import + the real DoH handle check), while the
    local-nest twin uses the fixture-setup ``set_state`` carve-out against a
    throwaway nest that has no handle in DNS at all. Same seam, same legs, two
    environments — mirroring how the two test modules already differ.

    Imports of the driver stack are LAZY, inside :meth:`start`. This module's
    pure half is pinned by tier_1 tests with no process and no nest in sight, and
    a module-level ``from drivers import create_driver`` would drag the whole
    driver/bridge surface into that.
    """

    def __init__(
        self,
        app_name: str,
        *,
        name: str,
        run_token: str,
        path: Path,
        launch_config: dict,
        sign_in,
        folder: str,
        node_url: str,
        agent_data_dir: Path | None = None,
        expect_rescan_ms: int | None = None,
    ) -> None:
        self.name = name
        # The cadence this seat was launched with through `FAUNA_E2E_RESCAN_MS`,
        # which `await_ready` pins against the one its agent ADOPTED. None = the
        # caller chose none; the driver default is then nobody's declaration.
        self._expect_rescan_ms = expect_rescan_ms
        self.path = path
        self.run_token = run_token
        self.app_name = app_name
        self.node_url = node_url
        self._config = launch_config
        self._sign_in = sign_in
        self._folder = folder
        # Where this seat's own `fauna-sync-agent` writes its rolling log, when
        # the harness pinned it (windows). None on a platform whose agent state
        # lives inside the driver's private world — `agent_log_note` says so out
        # loud rather than omitting the section.
        self._agent_data_dir = Path(agent_data_dir) if agent_data_dir else None
        self._driver = None
        self._app = None
        # Basenames this seat has been OBSERVED holding. `observed_delete` uses it
        # to witness a present→absent transition rather than bare absence — see
        # its docstring for exactly how far that goes.
        self._ever_held: set[str] = set()

    # ── lifecycle ────────────────────────────────────────────────────────────

    def start(self) -> None:
        from actions import ActionLayer
        from drivers import create_driver

        # Imported here rather than at module scope for the same reason as the
        # driver stack: it pulls the tri-machine module, which is a test module.
        import tests.test_filesync_multiseat_live as ms

        self.path.mkdir(parents=True, exist_ok=True)
        self._driver = create_driver(self.app_name)
        self._driver.launch(self._config)
        self._app = ActionLayer(self._driver)

        self._sign_in(self._app, self.name)
        ms._wait_connected(self._app, self.name)
        b = self._app.backups
        ms._adopt_or_create_set(self._app, b, self.name, set_name=self._folder)
        # Binding is what starts this seat's engine, so it is the last setup step
        # and `await_ready` polls for its effect rather than for a log line.
        ms._bind_folder(self._app, b, self.name, self.path, set_name=self._folder)

    @property
    def app(self):
        """The seat app's action layer, for a test that drives a page beside
        the sync legs (mail settings before a deposit is adopted, say)."""
        assert self._app is not None, f"[{self.name}] the seat is not started"
        return self._app

    def stop(self) -> None:
        if self._driver is not None:
            self._driver.teardown()
            self._driver = None
            self._app = None

    def stop_keeping_state(self) -> None:
        """Quit the app — and with it this seat's agent — keeping everything it
        wrote, so :meth:`restart` brings the SAME device back: its ``sync.db``
        and anchor, its binding, its signed-in store.

        The driver's own teardown, never a kill: it reaps the agent the app
        spawned as a group child (``drivers/tui.py::teardown``). The pin is
        what keeps the private launch world across it — without it the
        relaunch would be a fresh device catching up from an empty anchor,
        which is first sync, not catch-up.
        """
        assert self._driver.preserve_state_across_relaunch(), (
            f"[{self.name}] the {self.app_name} driver cannot keep its launch "
            f"state across a relaunch, so a restarted seat would be a new device "
            f"and a catch-up assertion would prove nothing"
        )
        self._driver.teardown()

    def restart(self, budget: float) -> float:
        """Relaunch after :meth:`stop_keeping_state` and wait until the engine
        serves the bound folder again; seconds waited.

        ``hard_reload`` on a pinned store waits for the app's own auto-login
        (a real user's restart), and the agent resumes the binding it kept.
        """
        self._driver.hard_reload()
        return self.await_ready(budget)

    def agent_log_lines(self) -> list[str]:
        """Every line of this seat's agent log, oldest file first — the engine's
        own account of what it applied, skipped or declined."""
        found, _ = self._find_agent_logs()
        lines: list[str] = []
        for path in found:
            try:
                lines += path.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                pass
        return lines

    def local_conflicts(self, conflict_type: str) -> list[tuple[str, str | None]]:
        """``(path, details)`` of this seat's ``sync_conflicts`` rows of one type.

        Read off the agent's own state DBs (``fsid-<ref>.db`` per folder,
        ``fauna_core::folder_keys::FolderRef::state_db_path``, under the
        launch's private world), read-only — every DB under its ``sync`` tree,
        so a rename of the per-folder file cannot quietly empty the read.
        That table is where the engine records a change it will never apply
        (`file-sync.md` § 5); no app renders it yet, so the row itself is the
        observable — the review list reads the nest's ``conflicts.list``,
        which this row never reaches.
        """
        import sqlite3

        config_home = getattr(self._driver, "config_home", None)
        assert config_home, f"[{self.name}] the driver exposes no launch root to search"
        dbs = sorted(Path(config_home).parent.glob("**/sync/**/*.db"))
        assert dbs, f"[{self.name}] no state DB under {Path(config_home).parent}"
        rows: list[tuple[str, str | None]] = []
        for db in dbs:
            conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            try:
                rows += conn.execute(
                    "SELECT path, details FROM sync_conflicts WHERE conflict_type = ?",
                    (conflict_type,),
                ).fetchall()
            except sqlite3.OperationalError:
                pass  # a state DB from another plane, with no conflicts table
            finally:
                conn.close()
        return rows

    def __enter__(self) -> "AppSeatDriver":
        self.start()
        return self

    def __exit__(self, *exc) -> None:
        self.stop()

    # ── observation ──────────────────────────────────────────────────────────

    def _sync_state(self) -> dict:
        """``data.sync`` — ``{running, locations}`` — or ``{}`` if unreadable."""
        try:
            state = self._driver.get_state() or {}
        except Exception:
            return {}
        return (state.get("data") or {}).get("sync") or {}

    def _held(self) -> set[str]:
        """This run's files currently in the folder, recording what it sees."""
        try:
            names = {p.name for p in self.path.glob(f"{self.run_token}-*")}
        except OSError:
            return set()
        self._ever_held |= names
        return names

    def describe(self) -> str:
        return (
            f"[multiseat] {self.name}: {self.app_name} app seat — real "
            f"{self.app_name} app + its child fauna-sync-agent -> "
            f"{self.node_url}, set {self._folder!r}, watch {self.path}"
        )

    def await_ready(self, budget: float) -> float:
        """Poll until this seat's engine is serving its bound folder.

        The app's own ``data.sync.running`` is the witness — it is
        ``any_engine_serving()`` on the app side, i.e. the agent answered and has
        an engine up for a bound folder. A green start returns on the first poll
        and pays none of the budget (convention 14).
        """
        started = time.monotonic()
        deadline = started + budget
        last: dict = {}
        while time.monotonic() < deadline:
            last = self._sync_state()
            if last.get("running"):
                self._await_adopted_rescan(deadline)
                return time.monotonic() - started
            time.sleep(1.0)
        raise AssertionError(
            f"[{self.name}] the {self.app_name} app never reported a running "
            f"sync engine within {budget:.0f}s — the folder is bound but the "
            f"agent never came up or never took the binding, so every leg below "
            f"would fail for a reason that has nothing to do with "
            f"convergence.\n{self.diagnostics()}\n"
            f"  last data.sync: {last!r}"
        )

    def _await_adopted_rescan(self, deadline: float) -> None:
        """Pin the cadence this seat's agent ADOPTED to the one it was launched
        with — a no-op when the caller chose none.

        The regression pin the daemon seat's applied-row line used to carry: the
        cadence went dead config twice (a TOML value the daemon ignored for the
        nest row, then a row every seat ignored for the phase-5 constant), and
        both times every convergence verdict meant something other than what the
        test said. The agent logs the armed value once per folder
        (:func:`adopted_rescan_ms`); the tick arms after the engine is serving,
        so this polls the same deadline ``await_ready`` was given.
        """
        want = self._expect_rescan_ms
        if want is None:
            return
        adopted: set[int] = set()
        while True:
            found, _ = self._find_agent_logs()
            lines: list[str] = []
            for path in found:
                try:
                    lines += path.read_text(encoding="utf-8", errors="replace").splitlines()
                except OSError:
                    pass
            adopted = adopted_rescan_ms(lines)
            if adopted or time.monotonic() >= deadline:
                break
            time.sleep(1.0)
        assert adopted == {want}, (
            f"[{self.name}] this seat was launched with a {want} ms rescan "
            f"cadence (FAUNA_E2E_RESCAN_MS), but its agent armed "
            f"{sorted(adopted) or 'no tick at all'} — so the reconcile backstop "
            f"is NOT running at the rate this test declares, and every verdict "
            f"below means something other than what it says. The seam is "
            f"compile-gated (always_resident::rescan_interval): check that the "
            f"agent is a debug / e2e-agent build and that the env reached it.\n"
            f"{self.agent_log_note()}"
        )

    def self_note(self) -> str:
        """"Am I the broken seat?", from evidence THIS seat holds.

        An app seat has no daemon log to parse (the agent logs into the launch's
        own private data dir, not the app's stderr), so the evidence is the app's
        own sync state plus what is on this seat's disk — never a guess about the
        peer (testing.md § point 6).
        """
        sync = self._sync_state()
        held = sorted(self._held())
        mine = [n for n in held if f"-hello-{self.name}." in n]
        return (
            f"  seat {self.name} ({self.app_name}) self-check: engine "
            f"running={sync.get('running')!r}, folders bound="
            f"{len(sync.get('folders') or [])}\n"
            f"    this run's files on my disk: {', '.join(held) or '(none)'}\n"
            f"    of which written by me: {', '.join(mine) or '(none)'}"
        )

    def observed_delete(self, basename: str) -> bool:
        """Has this seat applied the peer's delete of ``basename``?

        ⚠ **This degrades honestly and the degradation is real.** The retired engine seat
        answered from a positive log event (``applied remote delete``); an app seat
        has no such channel, so the only witness available is the file's absence
        from :attr:`folder`. Absence is a weaker observation than an applied
        event: on its own it is also satisfied by a file that never arrived.

        So it is strengthened as far as the seat honestly can: True requires the
        basename to have been OBSERVED here at some point (:meth:`_held` records
        every read) and to be absent now — a witnessed present→absent
        transition. If this seat never once saw the file, the answer stays False
        and the cleanup leg fails naming it, which is the correct outcome: a
        file that never arrived is exactly the failure the leg exists to catch.

        The present half is NOT left to polling luck: the cleanup leg records it
        on every watcher *before* the deleter unlinks
        (`convergence_legs.prime_delete_witness`), because a poll window that
        opens after the apply — routine for the second watcher, whose window
        opens only when the first watcher's whole await completes — would leave
        the transition unwitnessed and red the leg on a healthy product
        (``[3seat-tui+tui+tui]``'s first run, 2026-08-03; a false RED, never a
        false green, but a defunct verdict all the same).
        """
        held = self._held()
        if basename in held:
            return False
        return basename in self._ever_held

    def review_rows(self) -> list | None:
        """The Folders page's conflict review list, as ``(badge, info)``.

        The page reloads its snapshot whenever it becomes visible, so this
        navigates AWAY and back rather than trusting whatever was rendered
        earlier — the same refresh `test_devices_conflicts.py` uses, and the
        reason a stale read cannot quietly report zero rows.

        ``conflict-type-badge`` and ``conflict-file-info`` are both flat and
        one-per-conflict (`ui-actual-tui.yaml` § folders), so index `i` is the
        same row in both. Deliberately NOT indexed off `conflict-resolve-button`
        — that one is present only on rows retaining a non-winning candidate, so
        it would silently mis-pair every row after the first informational one.
        """
        # A NAMED generous budget, not the driver's 10 s default (convention 14).
        # This is a UI navigation on a real app on a heavily loaded machine:
        # 10 s is a guess at how long a WinUI page swap takes on an idle box, and
        # losing that guess reds the run at leg 4b with the whole convergence
        # already proven — which is exactly what happened on 2026-08-02 (the
        # sibling `native+engine` pair passed leg 4b in the same run). A green
        # navigation still returns on the first poll and pays none of this.
        self._driver.set_state(
            {"nav": {"stack": [{"view": "feed"}]}}, timeout=_REVIEW_NAV_BUDGET_S
        )
        self._app.backups.navigate_folders()
        # Read a STABLE snapshot: the page re-renders off a live machine
        # snapshot, so a tick landing between the count and the indexed reads
        # would make `get_text(index=n-1)` address a row that no longer exists.
        # Retry until the count is unchanged across the read rather than
        # tolerating a short read — silently returning fewer rows than the page
        # holds is how a review assertion goes quietly vacuous.
        for _attempt in range(_REVIEW_READ_ATTEMPTS):
            before = self._driver.count("conflict-file-info")
            rows = [
                (
                    self._driver.get_text("conflict-type-badge", index=i),
                    self._driver.get_text("conflict-file-info", index=i),
                )
                for i in range(before)
            ]
            if self._driver.count("conflict-file-info") == before:
                return rows
        raise AssertionError(
            f"[{self.name}] the conflict review list kept changing under the "
            f"read: {_REVIEW_READ_ATTEMPTS} attempts and the row count never "
            f"held still. That is not a convergence verdict — treat it as a "
            f"harness/render problem, not a sync one.\n{self.diagnostics()}"
        )

    def _find_agent_logs(self) -> tuple[list[Path], str | None]:
        """This seat's agent log files, and the place they were looked for.

        ``(paths, None)`` means there was nowhere to look — the only arm that
        may report "cannot locate"; every other outcome names a real directory,
        so "I looked and it is empty" stays distinguishable from "I could not
        look" (convention 7, one layer down).

        Two ways a seat's agent log is found, in order:

        - **the pinned dir**, when the harness named one (windows, whose agent
          rendezvouses on a machine-global ``%LOCALAPPDATA%`` and must be told
          where to keep state); and
        - **the driver's own private launch world** otherwise. This is the arm
          that was missing until 2026-08-28, and its absence cost a real
          diagnosis: on tui and macOS the note read "not pinned", so the one
          artifact that explains the seat's RECEIVE path had to be recovered by
          hand with ``lsof`` against the live process — during the 180 s window,
          or not at all. Nothing needed pinning; the log was always inside the
          launch's private dirs, which the driver already exposes. The agent's
          state root is platform-shaped below that (``Library/Application
          Support/Fauna/sync`` on macOS, ``<XDG_DATA_HOME>/fauna/sync`` on
          linux), so the search keys on the one component every platform shares
          — the agent's own ``sync/logs`` leaf — instead of re-deriving each
          platform's data-dir rule here, where it would silently rot.
        """
        if self._agent_data_dir is not None:
            logs = self._agent_data_dir / AGENT_LOG_SUBDIR
            try:
                return sorted(logs.glob(AGENT_LOG_GLOB)), str(self._agent_data_dir)
            except OSError:
                return [], str(self._agent_data_dir)

        # `config_home` is the per-launch isolated config dir every app driver
        # exposes; its parent is the launch's private root, which also holds the
        # private HOME/XDG_DATA_HOME the agent actually writes under.
        config_home = getattr(self._driver, "config_home", None)
        if not config_home:
            # The macOS driver (apple has no `config_home` at all) publishes the
            # agent's own state root instead — `<launch HOME>/Library/Application
            # Support/Fauna/sync`, the very directory the `logs` leaf sits under.
            state_base = getattr(self._driver, "sync_agent_state_base", None)
            if not state_base:
                return [], None
            base = Path(state_base)
            try:
                return (
                    sorted(base.glob(f"**/{AGENT_LOG_SUBDIR}/{AGENT_LOG_GLOB}")),
                    str(base),
                )
            except OSError:
                return [], str(base)
        root = Path(config_home).parent
        try:
            return (
                sorted(root.glob(f"**/sync/{AGENT_LOG_SUBDIR}/{AGENT_LOG_GLOB}")),
                str(root),
            )
        except OSError:
            return [], str(root)

    def agent_log_note(self, n: int = _AGENT_LOG_TAIL_LINES) -> str:
        """The tail of THIS seat's own ``fauna-sync-agent`` log.

        The engine seat's daemon log has always been in the failure message; an
        app seat's was not, so the one artifact that explains the RECEIVE path —
        whether a delivered change was applied, skipped, or never arrived — was
        exactly the one missing (testing.md § convention 16, the  red).

        The two seats run different binaries on that path (headless
        ``fauna-sync.exe`` vs the app's child ``fauna-sync-agent``), which is
        precisely why the app seat's own log cannot be inferred from the peer's.

        Every arm returns a NAMED note. "I cannot look", "I looked and it is
        empty" and "here is the tail" must stay distinguishable — collapsing the
        first two into silence is convention 7's complaint one layer down.
        """
        found, looked_in = self._find_agent_logs()
        if looked_in is None:
            return (
                f"  seat {self.name} agent log: <not locatable on "
                f"{self.app_name} — the seat has no pinned agent dir and its "
                f"driver exposes no private launch root to search>"
            )
        if not found:
            return (
                f"  seat {self.name} agent log: no log under {looked_in} "
                f"— the agent wrote NOTHING, so suspect the spawn itself before "
                f"the sync path (App.xaml.cs::SpawnSyncAgentDetached)"
            )
        # `fauna_log` rolls daily, so a long run leaves several files and the
        # newest is the live one. The names sort chronologically by construction
        # (`fauna.log.<YYYY-MM-DD>`), so the last is the current day's.
        path = found[-1]
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as exc:
            return f"  seat {self.name} agent log: {path} unreadable ({exc})"
        # The app polls the agent over its pipe several times a second, and the
        # agent logs every request at DEBUG — left in, a 40-line tail is pure
        # `ListEngines` and the sync lines that explain a red never appear (the
        # 2026-09-22b 3-seat native delete red was undiagnosable for exactly
        # this reason). The count stays in the note, so a filtered tail never
        # reads as a silent agent.
        kept = [ln for ln in lines if not _is_pipe_poll_line(ln)]
        dropped = len(lines) - len(kept)
        tail = kept[-n:]
        body = "\n".join(f"    {ln}" for ln in tail) or "    <empty>"
        filtered = f" ({dropped} pipe-request DEBUG lines omitted)" if dropped else ""
        return f"  seat {self.name} agent log: {path}{filtered}\n{body}"

    def diagnostics(self) -> str:
        sync = self._sync_state()
        try:
            error = self._app.error_text()
        except Exception:
            error = "<unreadable>"
        held = sorted(self._held())
        return (
            f"  seat {self.name}: {self.app_name} app, engine "
            f"running={sync.get('running')!r}, "
            f"folders={sync.get('folders')!r}\n"
            f"  this run's files in {self.path}: "
            f"{', '.join(held) if held else '(nothing)'}\n"
            f"  ever seen here: "
            f"{', '.join(sorted(self._ever_held)) if self._ever_held else '(nothing)'}\n"
            f"  app error-message element: {error!r}\n"
            f"{self.agent_log_note()}"
        )


__all__ = [
    "AGENT_LOG_GLOB",
    "AGENT_LOG_SUBDIR",
    "DEFAULT_PAIRS",
    "DEFAULT_TRIOS",
    "NATIVE_APPS",
    "RESOLVED_BADGES",
    "RUN_TOKEN_RE",
    "SEAT_MODES",
    "SEAT_NAMES",
    "SEAT_SETS_ENV",
    "UNBUILT_MODES",
    "UNSUPPORTED_MODES",
    "AppSeatDriver",
    "SeatDriver",
    "finalize_live_residue",
    "isolate_windows_sync_agent",
    "make_seat",
    "native_app_for_platform",
    "new_run_token",
    "pin_macos_seat_sync_agent",
    "residue_note",
    "review_rows_for_run",
    "seat_names",
    "seat_set_id",
    "seat_sets_for_platform",
    "seat_sets_from_env",
    "start_seats",
    "token_prefix",
    "unbuilt_note",
    "unresolved_review_rows",
    "windows_agent_data_dir",
]

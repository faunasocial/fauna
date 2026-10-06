"""N sync seats on ONE machine — seat count × seat-mode set × nest mode, one family.

The seat-count fold (2026-08-03): ``test_filesync_twoseat.py`` and
``test_filesync_threeseat.py`` differed only in their matrix constant and their
prose, so they are one module now — the end state of the nest-mode axis's
collapse (the ratified axis design: `docs/goal/architecture/testing.md`
§ Default app and nest mode, *Mode mechanics*; the N-seat shape: § convention
16). The convergence legs have one owner (``helpers/convergence_legs.py``), the
matrix has one owner (``sync_seats.DEFAULT_SEAT_SETS`` — every mode-uniform set
at both ratified seat counts), and the nest is whatever ``provider_for(run
mode)`` fills the slot with:

* ``--nest standalone`` (the default): a freshly built local ``fauna-nest`` —
  the inner-loop red-green instrument. A nest-side sync change goes green here
  first.
* ``--nest docker[:IMAGE]``: the nest **image** under s6 supervision — the same
  legs through the *deployed artifact* shape. This is the design record's
  named payoff: a sync fix gated behind a production redeploy is proven in
  image shape here, with no release dispatched. (The image always serves TLS
  with its self-signed floor cert; the seats dial ``https://127.0.0.1:<port>``
  and ride the client transport's loopback trust short-circuit — a red naming
  TLS here is a real finding about that short-circuit, not harness noise.)
* ``--nest live[:URL]``: the **deployed** nest on the shared live box (default
  example.com) — the only mode with real DNS, a real CA-issued cert, a real
  network path and accumulated state, so the only one that fails a
  works-on-a-fresh-box-only bug.

Selecting a seat count — what the per-count files used to do — is ``-k``:
every parametrization id leads with the count (``[2seat-tui+tui]``,
``[3seat-tui+tui+tui]``; ``sync_seats.seat_set_param_id``), the same
``<N>seat-`` prefix the run token and any live-set residue carry, so
``-k 2seat`` runs the pairs and a leftover ``3seat-…`` file on the shared set
names the exact cells to re-run. Mixed diagnostic sets stay env-opt-in
(``$FAUNA_SEAT_SETS``; the set's length is the seat count).

The one thing live does differently: SIGN-IN
--------------------------------------------
Convention 16 rules that a live UI seat signs in through the real UI, and the
``set_state`` carve-out justifies itself by a fact that stops being true there —
*a harness nest has no handle in DNS*. So the modes branch exactly once, on
which ``sign_in`` is handed to ``sync_seats.make_seat``: :func:`live_sign_in`
or :func:`harness_sign_in`.

That works on a FRESH account — the non-obvious part — because the handle
check's DoH phase resolves the handle's **domain**, never its localpart, and
what binds handle to account is the silent challenge that reads back the handle
the nest stored. So an account provisioned seconds ago answers
``AlreadyOnNest { handle_matches: true }`` like any other. The account is
consequently registered **with** a handle in every mode (:func:`handle_for`).

What three seats prove that two do not
--------------------------------------
Two seats can only ever state **pairwise** facts, and a real account has more
than two devices. Three add exactly the properties two cannot express:

* **Fan-out.** A write must reach *every* peer, not "the other one". The hello
  leg is six observations there rather than two, and it includes ``a → c`` — a
  delivery rail that fanned out to only the first-registered device would pass
  every two-seat cell and fail a real account.
* **A merge of two remote edits.** Each seat must fold **two** independent
  remote edits from the common base into its own and still land on identical
  bytes — the case where an engine that resolves pairwise but not transitively
  diverges. (This exact leg was RED through 2026-08-02 on a real product
  defect: with one scalar merge ancestor and no causality on the wire, the
  authoring seat read ``local == base`` as "I have not diverged" and lost its
  own edit while logging ``conflict auto-resolved: clean merge applied``.
  Closed by the causal watermark — `conflicts.md` clause 5,
  ``SyncChange::derived_through``. It is the leg to distrust first if a
  ``3seat-`` cell ever reds again; do not re-derive the four refuted merge-base
  policies — they are kept as executable refutation records in
  ``libs/fauna-sync-engine/src/merge_convergence_test.rs``.)
* **Delete propagation to more than one peer.** A tombstone reaching one device
  and not another is invisible to a two-seat run; here every peer is watched
  applying every deletion.

The legs are written over the seat *list* and the fan-out is enumerated by
``convergence_legs.hello_fanout_plan`` rather than by neighbour indexing —
which is what stops a three-seat assertion silently narrowing back into two
pairwise ones. Timeout ceilings derive from ``convergence_legs.await_count(n)``
per cell, so a leg added to the plan can never silently under-bound a count.

What a green run proves — and what it does not
----------------------------------------------
Proves: upload, change feed, download, concurrent merge and delete propagation,
end to end between N devices of one account, through a real nest — with **no**
``folder_destinations`` rows, i.e. the configuration a real user actually
reaches (a phantom `folder_destinations` rail once let a total device-to-device
sync failure sit behind a green suite, which is why that qualifier
is load-bearing). In docker mode, additionally: that the published image's own
binary, supervision tree and boot gates serve that round trip.

In live mode, additionally: that the *deployed* box does — including the real
onboarding sign-in each UI seat takes to get there. Isolation there is
account-scoped: the run provisions its own account and set and reaps them
(``helpers/live_accounts.py``), the run token namespaces every file it writes,
and the assertive cleanup leg still runs, because convention 16 makes it a
condition of the standing unattended authorization rather than a nicety.

Does not prove: anything about another OS or real cross-machine networking.
**The tri-machine round is not retired and its explicit-`go` iron-clad is
untouched** — it remains the certification run for cross-OS path and filesystem
heterogeneity, real cross-machine network diversity and staggering, and the
other platforms' agent-spawn seams. Nothing here licenses skipping its gate.

Running it::

    pytest tests/test_filesync_seats.py -s                     # standalone, all cells
    pytest tests/test_filesync_seats.py -s -k 2seat            # pairs only
    pytest tests/test_filesync_seats.py -s --nest docker:ghcr.io/faunasocial/nest:latest
    pytest tests/test_filesync_seats.py -s --nest live
"""

from __future__ import annotations

import contextlib
from dataclasses import dataclass
import secrets
import shutil
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlparse

import pytest

import os

from common import (
    create_folder,
    register_user,
)
from common.auth import UNBINDING_MAX_DEVICES, set_tier_caps, user_create_folder
from helpers import convergence_legs
from common.accounts import actor_id_hex
from helpers import nest_mode as nm
from helpers import seat_scenarios
from helpers import sync_seats

# NOTE: this module must never write `FAUNA_LIVE_NEST_URL` at import time, the
# way the retired live twins did. That env var is one of the two things that
# engage conftest's machine-wide live-box flock, and setting it here would make
# a plain `--nest standalone` run serialize on the shared live box. Under
# `--nest live` the flock engages from the MODE instead
# (`conftest._serialize_live_box`) — a run-level fact no module import forges.

# Ceilings a green run never pays (convention 14 — deadline polls, never
# settle-sleeps). NOT tightened for loopback: 20+ concurrent sessions is this
# machine's normal state, and a tight budget would turn this into exactly the
# load-sensitive test the box cannot host. The same ceilings serve docker mode:
# container boot is seconds, and the delta is absorbed by STARTUP_WINDOW.
WINDOW = 180.0
STARTUP_WINDOW = 180.0


# ── the reconcile backstop's cadence: deliberately unreachable ────────────────
# This cell is a test of the REAL-TIME change-notification path — each seat's
# watcher (FSEvents / inotify / ReadDirectoryChangesW), its debouncer, and the
# nudge that carries a peer's change. `rescan_interval_secs` is a *different*
# mechanism: the periodic full-reconcile backstop that re-lists the folder to
# catch watcher misses (`file-sync.md` § Config, "what scan frequency actually
# means"). Letting it fire inside a leg would make this cell unable to tell a
# working watcher from a broken one repaired within seconds — and that is not
# hypothetical: it is exactly the coverage that found both defects this cell has
# ever found. The swallowed delete and the three-daemon
# blackout are both invisible to a run whose backstop
# sweeps every few seconds, and the landed fix runs its delete sweep ON the
# rescan tick — so a short cadence would turn its e2e proof green without ever
# exercising the debounce-drain trigger that is the half a user actually feels.
#
# So the default is derived from the run's OWN ceiling rather than picked: a
# cadence the longest cell cannot reach cannot fire mid-run, by construction, and
# it stays true if the leg plan grows. This is a deliberate choice of what the
# cell measures, not an accident — which is what the previous state was, twice.
# The harness wrote 5 into each daemon's TOML, the daemon correctly discarded it
# for the nest row's 300, and the knob was dead config for the cell's whole
# life; then phase 5 of the folders re-model (2026-08-20) made the cadence a
# hard-coded constant every seat ignores the row for, and this cell's row write
# went dead the same way. The cadence now rides the engine's compile-gated
# `FAUNA_E2E_RESCAN_MS` seam (`always_resident::rescan_interval`), set per seat
# by `sync_seats.make_seat(rescan_secs=...)` — the only way a run chooses it.
#
# `FAUNA_E2E_SEATS_RESCAN_SECS` overrides it for a run that wants the backstop
# ON — the discriminating experiment is the motivating case: give the cell a
# genuinely short cadence and re-run, and whether the wedged daemons recover
# splits "the watcher callback thread is stuck" from "the whole select! loop is
# stuck". Harness-side only; the product's cadence is a constant (`file-sync.md`
# § Config, the phase-5 block).
_RESCAN_ENV = "FAUNA_E2E_SEATS_RESCAN_SECS"


def _rescan_secs_for(seat_count: int) -> int:
    """The cadence this run sets on every seat's seam — unreachable by default."""
    override = os.environ.get(_RESCAN_ENV, "").strip()
    if override:
        secs = int(override)
        if secs <= 0:
            raise ValueError(
                f"{_RESCAN_ENV}={override!r}: the cadence must be positive — the "
                f"seam falls back to the constant for a non-positive value, so "
                f"this would silently not apply."
            )
        return secs
    # +1 so it is strictly greater than the ceiling, never equal to it.
    return _timeout_for(seat_count) + 1


def _timeout_for(seat_count: int) -> int:
    """A cell's ceiling, derived from ITS leg plan — never from a hand-counted
    number that silently stops matching it (convention 9 — bounded always, and
    bounded by something true). Carried per parametrization cell rather than
    module-wide, so a two-seat cell is not bounded by the three-seat plan."""
    return int(
        seat_count * STARTUP_WINDOW
        + convergence_legs.await_count(seat_count) * WINDOW
        + convergence_legs.REVIEW_BUDGET_S
        + 300
    )


def _set_name_for(seat_count: int) -> str:
    """This cell's folder name — ``2seat``/``3seat``, the same count-leading
    vocabulary as the run token and the parametrization id. Per-count so a log
    line or UI row names the run shape; per-run uniqueness comes from the nest
    itself (standalone/docker are fresh) or the run's own account (live)."""
    return f"{seat_count}seat"


pytestmark = [
    pytest.mark.tier_3,  # the authored FLOOR; docker mode elevates to tier_4 at collection
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
    pytest.mark.tui,
]


def handle_for(actor_id: str) -> str:
    """This run's handle, derived from the actor id.

    Same derivation `common.auth.create_actor_and_register` uses, and for the
    same reason: unique on the nest by construction (so repeated runs against
    the shared live box never collide) and a valid handle — 3–63 chars,
    lowercase alphanumeric plus hyphens (`public-mode.md` § User Registration).

    Registered in EVERY mode, not just live. A handle is what lets the live
    seats sign in through the real UI at all (:func:`live_sign_in`), and there
    is no reason for the harness-nest modes to register a *different* shape of
    account than the one the live mode proves — priority #1's uniform shape.
    """
    return f"e2e-{actor_id[:12]}"


def live_sign_in(secret_key: str, address: str):
    """The REAL-UI sign-in a seat uses against the LIVE box (convention 16).

    Seed import plus the real DoH handle check — the production onboarding
    path, which is itself worth exercising (convention 8). The flow has one
    owner, `tests.test_filesync_multiseat_live._sign_in`; this passes *this
    run's own* account into it via that function's additive
    ``secret_hex``/``address`` overrides rather than copying the steps.

    **Why a freshly-provisioned account can use the production path at all** —
    the fact that made the live fold a one-line branch instead of a redesign:
    the handle check's DoH phase looks up the handle's **domain** only
    (`fauna-onboarding-machine/src/machine.rs::run_handle_check_phases`, phase
    2 — NS records + TLD validity), never the localpart. What binds a handle to
    an account is phase 4's silent challenge, which authenticates the imported
    secret and reads back the handle the nest stored. So a handle registered
    seconds ago answers ``AlreadyOnNest { handle_matches: true }`` exactly as a
    year-old one does, and routes to ``WizardOutcome::LoggedIn``.

    The corollary is a requirement, not a detail: the account must be
    registered **with** a handle (:func:`handle_for`). A handle-less one reads
    as ``already_on_nest_handle_differs`` and never reaches the feed.
    """

    def _sign_in(app, seat: str) -> None:
        # Imported lazily: `ms` is a test module, and only this branch needs
        # it — the same reason `sync_seats.make_seat` defers its driver stack.
        import tests.test_filesync_multiseat_live as ms

        # Every seat imports the SAME seed and still becomes its own device:
        # the device id is per-`sync.db`/per-app-store, and the driver's
        # private HOME/XDG world makes those per-seat (convention 10). So the
        # harness branch's explicit device-id split has no live counterpart.
        ms._sign_in(app, seat, secret_hex=secret_key, address=address)

    return _sign_in


def live_address_for(handle: str, node_url: str) -> str:
    """``handle@<box domain>`` — the address a live UI seat types.

    The domain comes from the RESOLVED node url rather than a constant, so
    ``--nest live:URL`` points sign-in at the same box the rest of the run hit.
    """
    return f"{handle}@{urlparse(node_url).hostname}"


def sign_in_for(nest_mode, *, node_url: str, secret_key: str, actor_id: str, handle: str):
    """The one branch between the modes — live signs in for real, others fake.

    One function for every seat count, so the counts cannot drift in how they
    authenticate.
    """
    if nest_mode.is_live:
        return live_sign_in(secret_key, live_address_for(handle, node_url))
    return harness_sign_in(node_url, secret_key, actor_id, handle)


def register_run_account(port, actor_id: str, *, node_url: str, admin_sk) -> str:
    """Register this run's account under a handle. Returns the handle.

    On live the account is recorded for the teardown reap by the WS-RPC wire
    core, which notes every account-creating kind it carries
    (`helpers/live_accounts.py`; `conftest._LiveProvider.start`'s cleanup reads
    the ledger) — no per-module note is owed.
    """
    handle = handle_for(actor_id)
    register_user(
        port, actor_id, base_url=node_url, admin_signing_key=admin_sk, handle=handle
    )
    return handle


def create_run_set(
    port, set_name: str, actor_id: str, *, node_url: str, secret_key: str, admin_sk, nest_mode
) -> None:
    """Create the run's folder **custody-first**, as its owner.

    Every seat's engine signs its change records under the set's nonce, and the
    nonce lives in the account's folder-key custody. `user_create_folder` (the
    shared `create_set`, through `fauna_ffi.harness_create_set`) mints it there
    and publishes it before the create, so each seat reads it off the account
    plane like any second device of the account does.

    The admin `create_folder` this used to call writes no custody entry. The
    only thing that mints a nonce for such a set afterwards is the owner app's
    launch-time set-custody reconcile, which rides the conversations session's
    receive loop — and macos and windows keep a mock conversations backend
    under e2e, so their native seats never ran it: the engine built with a
    signer and no nonce, recorded unsigned, and the nest refused every record
    `fauna.sync.signature_required` at the plan's first step (2026-10-01, both
    apps). tui and linux run the real loop for every login and minted one —
    once per seat, racing — which is not a shape worth keeping either.

    One mode still takes the admin create: a harness nest behind self-signed
    TLS (docker), which the harness seam cannot dial yet. There a seat still depends on its app's reconcile.
    """
    self_signed = urlparse(node_url).scheme == "https" and not nest_mode.is_live
    if self_signed:
        create_folder(
            port, set_name, actor_id, base_url=node_url, admin_signing_key=admin_sk
        )
        return
    user_create_folder(port, set_name, secret_key=secret_key, base_url=node_url)


def harness_sign_in(node_url: str, secret_key: str, actor_id: str, handle: str):
    """The ``set_state`` sign-in a seat uses against a HARNESS nest.

    A harness nest (standalone binary or container alike) has no handle in DNS,
    so the live onboarding flow (seed import + the REAL DoH handle check)
    cannot run here. This is the fixture-setup `set_state` carve-out every other
    tier_3 UI test logs in with (`conftest._login_app_as`; prior art for two
    same-actor tui launches: `test_filesync_bind_history.py::_launch_signed_in`).

    The device-id derivation below is the one line that makes the whole shape
    meaningful, and having it once for every seat count is the point of the
    fold: a second copy is a second place for it to go wrong.
    """
    from conftest import _E2E_LOGIN_DEVICE_ID

    def _sign_in(app, seat: str) -> None:
        app.driver.set_state(
            {
                "session": {
                    "authenticated": True,
                    "node_url": node_url,
                    "secret_hex": secret_key,
                    "handle": handle,
                    "actor_id": actor_id,
                    # Same identity, DIFFERENT device — the laptop+tablet
                    # scenario the whole multi-seat shape models. A shared
                    # device id would make the nest treat the seats as one
                    # device and the run would prove nothing.
                    "device_id": _E2E_LOGIN_DEVICE_ID[:-1] + seat,
                },
                "nav": {"stack": [{"view": "feed"}]},
            }
        )

    return _sign_in


@dataclass
class SeatRun:
    """What a seat test body gets: ready seats, the run token they share, and
    what a failure note needs."""

    seats: list
    run_token: str
    label: str


@contextlib.contextmanager
def _seat_run(seat_set, nest_mode, tmp_path_factory, request):
    """Bring up this cell's nest, account, set and seats; yield a :class:`SeatRun`.

    One setup for every test body in this module, so the convergence legs and
    the scenario legs can never drift apart on how a seat gets syncing, which
    nest mode it rides, or how the live box is cleaned up after it.
    """
    seat_count = len(seat_set)
    set_name = _set_name_for(seat_count)
    rescan_secs = _rescan_secs_for(seat_count)

    # Seat trees only — the nest's own dirs are the provider's to place and the
    # cleanup below is its to run. mkdtemp (not `tmp_path`): the watch dirs are
    # torn down in the same `finally` that stops the nest, and a fixture-managed
    # tree would outlive the process teardown order on some platforms.
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{set_name}-"))
    run_token = sync_seats.new_run_token(seats=seat_count)

    provider = nm.provider_for(nest_mode)
    # Resolved lazily and only where a local binary is what fills the slot — the
    # same rule `conftest.nest_instance` follows, and for the same two reasons:
    # docker/live never execute one, and DECLARING `nest_binary` would put it in
    # this test's fixture closure, which is now precisely what marks a test
    # standalone-only (`nest_surface.NEST_BINARY_FIXTURES`). This file rides the
    # provider, so it belongs in every mode; a declared `nest_binary` would have
    # quietly deselected it from docker and live.
    nest_binary = (
        request.getfixturevalue("nest_binary")
        if nm.builds_local_nest(nest_mode) else None
    )
    nest, nest_cleanup = provider.start(nest_binary, tmp_path_factory, set_name)
    node_url = nest["url"]
    port = nest["port"]
    admin_sk = nest["admin"]["signing_key"]
    secret_key = secrets.token_hex(32)

    print(
        f"[seats] mode={nest_mode.id} seats={sync_seats.seat_set_id(seat_set)} "
        f"token={run_token} set={set_name!r} nest={node_url} "
        f"window={WINDOW:.0f}s rescan={rescan_secs}s"
        f"{' (BACKSTOP ON — overridden)' if os.environ.get(_RESCAN_ENV, '').strip() else ' (backstop unreachable by design)'}",
        flush=True,
    )
    # No silent caps: a platform collecting fewer seat sets than the matrix
    # declares says which and why, so a green run is never mistaken for full
    # coverage.
    note = sync_seats.unbuilt_note(sys.platform)
    if note:
        print(note, flush=True)

    try:
        actor_id = actor_id_hex(secret_key)
        handle = register_run_account(port, actor_id, node_url=node_url, admin_sk=admin_sk)
        # Every seat registers its own device, and a multi-tenant nest enforces
        # the tier's device cap. On the `free` seed (2 devices) a 3-seat cell's
        # third register was refused `device_limit_exceeded`, and that seat ran
        # on as a device the nest refuses at every dial. Admit the account at a
        # cap that does not bind, as the session `test_user` fixture does; the
        # refusal itself is proven nest-side (`common.auth.set_tier_caps` names
        # the conformance file).
        set_tier_caps(
            port,
            admin_signing_key=admin_sk,
            max_devices=UNBINDING_MAX_DEVICES,
            base_url=node_url,
        )
        create_run_set(
            port,
            set_name,
            actor_id,
            node_url=node_url,
            secret_key=secret_key,
            admin_sk=admin_sk,
            nest_mode=nest_mode,
        )
        # The cadence is set on every seat's `FAUNA_E2E_RESCAN_MS` seam below
        # (`make_seat(rescan_secs=...)`) — never on the nest row, which no seat
        # has read
        # since phase 5 (the row write this cell used to make here went dead
        # the day the de-knob landed).

        sign_in = sign_in_for(
            nest_mode,
            node_url=node_url,
            secret_key=secret_key,
            actor_id=actor_id,
            handle=handle,
        )

        with contextlib.ExitStack() as stack:
            seats = sync_seats.start_seats(
                stack,
                seat_set,
                make_one=lambda mode, name: sync_seats.make_seat(
                    mode,
                    name=name,
                    run_token=run_token,
                    root=tmp / name,
                    node_url=node_url,
                    node_port=port,
                    folder=set_name,
                    sign_in=sign_in,
                    request=request,
                    rescan_secs=rescan_secs,
                ),
                startup_window=STARTUP_WINDOW,
            )

            try:
                yield SeatRun(
                    seats=seats,
                    run_token=run_token,
                    label=f"{nest_mode.id}/{sync_seats.seat_set_id(seat_set)}",
                )
            finally:
                # Only the shared box is owed anything back, and only while the
                # seats are still alive (a deleted file leaves the nest only if
                # a running daemon says so). A harness nest — binary or
                # container — is discarded whole below.
                if nest_mode.is_live:
                    sync_seats.finalize_live_residue(run_token, seats)
    finally:
        # On live this is what reaps the run's account, so it must run whether
        # the body passed, failed or died mid-leg.
        nest_cleanup()
        # The nest side is the provider's; the seat trees are this test's alone,
        # so the whole tree just goes.
        shutil.rmtree(tmp, ignore_errors=True)


@pytest.mark.parametrize(
    "seat_set",
    [
        pytest.param(
            seat_set,
            id=sync_seats.seat_set_param_id(seat_set),
            # Each cell's ceiling derives from ITS OWN leg plan — a module-wide
            # mark would bound every pair by the widest trio's plan.
            marks=pytest.mark.timeout(_timeout_for(len(seat_set))),
        )
        for seat_set in sync_seats.seat_sets_for_platform(
            sys.platform,
            sync_seats.seat_sets_from_env() or sync_seats.DEFAULT_SEAT_SETS,
        )
    ],
)
@pytest.mark.feature("local-folder-sync")
def test_seats_converge(seat_set, nest_mode, tmp_path_factory, request):
    """N seats of one account converge through this run's nest mode.

    Creates **no** ``folder_destinations`` rows — and cannot: that rail (table,
    its ``fauna.admin.folders.add_destination`` writer, and the orchestrated
    forward) was deleted outright 2026-08-18 precisely
    because no product code ever called it. A test that added them was testing a
    configuration no user reaches — the mistake that let a total device-to-device
    sync failure sit behind a green suite until 2026-08-01.
    """
    with _seat_run(seat_set, nest_mode, tmp_path_factory, request) as run:
        convergence_legs.run_convergence_legs(
            run.seats,
            run.run_token,
            window=WINDOW,
            label=run.label,
        )


def _scenario_timeout(seat_count: int) -> int:
    """The scenario cell's ceiling, derived from ITS plan (convention 9)."""
    return int(
        seat_count * STARTUP_WINDOW + seat_scenarios.step_count() * WINDOW + 300
    )


@pytest.mark.parametrize(
    "seat_set",
    [
        pytest.param(
            seat_set,
            id=sync_seats.seat_set_param_id(seat_set),
            marks=pytest.mark.timeout(_scenario_timeout(len(seat_set))),
        )
        # Pairs only: every scenario is a writer→peer fact, and the fan-out is
        # `test_seats_converge`'s job at three seats.
        for seat_set in sync_seats.seat_sets_for_platform(
            sys.platform,
            sync_seats.seat_sets_from_env() or sync_seats.DEFAULT_PAIRS,
        )
    ],
)
@pytest.mark.feature("local-folder-sync")
def test_seat_scenarios(seat_set, nest_mode, tmp_path_factory, request):
    """The file-shape cases of device-to-device sync, between signing app seats.

    A shrinking overwrite, delete-then-recreate, a batch, empty / NUL / CRLF /
    binary bodies, non-ASCII names, nested directories, a rename, rapid
    rewrites, a multi-chunk file, a delete travelling the other way and an
    overlapping concurrent edit — the shared-contract cases the retired
    headless-daemon suites proved until the daemon's unsigned records
    became unrecordable (the nest refuses them) (``helpers/seat_scenarios.py`` names each
    one's former home). Same seats, same nest modes and same live-box cleanup
    as :func:`test_seats_converge`, through :func:`_seat_run`.
    """
    with _seat_run(seat_set, nest_mode, tmp_path_factory, request) as run:
        seat_scenarios.run_scenarios(
            run.seats,
            run.run_token,
            window=WINDOW,
        )

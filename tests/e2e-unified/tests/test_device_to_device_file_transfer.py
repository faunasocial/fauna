"""Files move straight between two of your own devices — and still arrive when
the direct path is gone.

Owner: ``docs/goal/behavior/p2p.md`` § Goal (the three promises) and
§ Implementation status today (the same-account peer data plane): a file-sync
download in the per-user sync agent asks this account's other devices for its
chunk bodies before the nest, over the channels the peer leg's dial pass
admitted, and fetches from the nest only what no sibling served
(``fauna_sync_engine::sibling_source::sibling_first``).

**What this pins that the tier below cannot.** The mechanism is pinned one tier
down (``fauna-sync-engine/tests/peer_dial_convergence.rs``): a body crosses a
dialed leg, the rest is left to the nest, a forged body is refused. Every one of
those is handed its siblings. What none of them can answer is whether two real
seats — each a real app with its own sync agent, signed in to one account — find
each other with nobody pairing them, and whether the agent that holds the
machine's engine role beside a running app is the one whose peer leg carries
the bytes (``account-runtime.md`` § Multi-instance concurrency → *The agent
holds the role when present*).

**The observable** is the receiving agent's own reading of the plane — its
``GetServiceStatus`` reply's ``peer_files`` (``fauna_ipc::sync::PeerFilesInfo``):
how many siblings its peer leg holds, how many chunk bodies a sibling served,
how many it left to the nest. "The nest never sees the data" is a claim about
the file's BYTES: while the nest answers, manifests and the change feed stay
nest-mediated by design (``p2p.md`` § Implementation status today); with it
down, a sibling serves those too and the nest sequences the row on its return
(``p2p.md`` § Same-account nest-down folder transfer). So both direct journeys use a
**metadata-only** folder, whose bytes never rest on the nest — the only other
road for them is the nest's relay, and a reading of every chunk served by the
sibling and none left to the nest says they did not take it.

Two seats on one machine, no operator (convention 16). Every wait is a deadline
poll on state (convention 14).

**Which seats.** Each platform runs its own pair through the one `make_seat`
seam: tui's where tui drives a sync agent, the platform's own GUI app
(``native``) where its leg is built. The plane lives in the per-user agent every
desktop app spawns, so a native pair is the same journey with that app's agent
up — on windows a per-seat agent on its own pipe, which outlives its app by
design and is stopped through its own ``Shutdown`` when a seat goes away
(``AppSeatDriver._stop_surviving_agent``).
"""

import contextlib
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from common import register_user, set_folder_residency, user_create_folder
from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, sealed_path, set_tier_caps, sync_changes_list
from common.nest import start_nest_in_place, stop_nest
from helpers import sync_seats
from helpers.folder_content import _log_path, atomic_write
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (`p2p.md` § Implementation status today); windows and linux
    # run their native pairs. macOS joins as its native leg lands — an owed
    # leg is filed in `OWED_SEATS` below, never skipped.
    pytest.mark.tui,
    pytest.mark.windows,
    pytest.mark.linux,
]

# This journey's own owed legs, in `sync_seats.UNBUILT_MODES`' shape. The native
# seats themselves are built on both platforms (they sync in the seat module);
# what is owed is THIS journey's reading of their agent and their keep-state
# stop on that app. A landed leg deletes
# its entry here.
OWED_SEATS = {
    "darwin": {
        "native": "the macOS app's leg of the device-to-device transfer — its "
                  "agent's peer reading and its keep-state stop are unproven",
    },
}


def seat_pairs(sys_platform: str) -> tuple:
    """The seat pairs this journey runs on ``sys_platform``."""
    return sync_seats.seat_sets_for_platform(
        sys_platform, sync_seats.DEFAULT_PAIRS, unbuilt=OWED_SEATS
    )


_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in seat_pairs(sys.platform)
]

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
# The peer leg admits a sibling on a pass of the account runtime, which an
# account-plane change (the sibling publishing its dial candidates) wakes; the
# backstop is five minutes, so the window covers one full backstop.
SIBLING_WINDOW = 330.0
WINDOW = 180.0
# The nest's return: the agents' reconnect backoff, then an upload pass.
RETURN_WINDOW = 300.0

# Over one 1 MiB peer frame, so the sibling's chunk crosses in ranged slices
# (`fauna_peer_sync::ranged`) rather than in one reply.
BODY_BYTES = 3 * 1024 * 1024 + 17


def _account(nest_instance):
    from tests.test_filesync_seats import handle_for

    port, url = nest_instance["port"], nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = handle_for(actor_id)
    register_user(port, actor_id, base_url=url, admin_signing_key=admin_sk, handle=handle)
    # Two seats are two devices; never let the tier cap refuse one.
    set_tier_caps(port, admin_signing_key=admin_sk, max_devices=UNBINDING_MAX_DEVICES,
                  base_url=url)
    return port, url, secret_key, actor_id, handle


def _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair):
    from tests.test_filesync_seats import harness_sign_in

    port, url, secret_key, actor_id, handle = account
    modes = dict(zip(sync_seats.seat_names(len(seat_pair)), seat_pair))
    note = sync_seats.unbuilt_note(sys.platform, unbuilt=OWED_SEATS)
    if note:
        print(note, flush=True)

    def seat(seat_name: str):
        return sync_seats.make_seat(
            modes[seat_name],
            name=seat_name,
            run_token=run_token,
            root=tmp / seat_name,
            node_url=url,
            node_port=port,
            folder=folder,
            sign_in=harness_sign_in(url, secret_key, actor_id, handle),
            request=request,
        )

    return seat


def _reading(seat) -> dict:
    return seat.peer_files() or {}


def _await_siblings(seat, other, what: str) -> None:
    """Wait until ``seat``'s agent holds an admitted channel to a sibling."""
    wait_until(
        lambda: _reading(seat).get("siblings", 0) >= 1,
        SIBLING_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"{what}: seat {seat.name}'s agent never admitted a sibling device over "
            f"the peer leg (last reading {seat.peer_files()!r}) — the direct path "
            f"never existed, so nothing below could prove it\n"
            f"--- seat {seat.name} ---\n{seat.diagnostics()}\n"
            f"--- seat {other.name} ---\n{other.diagnostics()}"
        ),
    )


def _await_bytes(seat, name: str, body: bytes, what: str, *others) -> None:
    target = seat.path / name

    def landed() -> bool:
        try:
            return target.read_bytes() == body
        except OSError:
            return False

    wait_until(
        landed, WINDOW, interval=1.0,
        diagnose=lambda: (
            f"{what}: {name!r} never reached seat {seat.name} byte for byte "
            f"(its plane reading {seat.peer_files()!r})\n"
            f"--- seat {seat.name} ---\n{seat.diagnostics()}\n"
            + "".join(f"--- seat {o.name} ---\n{o.diagnostics()}\n" for o in others)
        ),
    )


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + SIBLING_WINDOW + 2 * WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_a_file_moves_straight_between_two_of_your_devices(nest_instance, request, seat_pair):
    """Two seats of one account, both up, nothing paired or switched on: a file
    written on seat ``a`` into a metadata-only folder lands on seat ``b`` with
    every chunk served by ``a``'s agent and none left to the nest."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"direct-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    # Metadata-only BEFORE any engine first sees the folder: no seat ever
    # uploads these bytes, so the nest never holds them even in passing.
    reply = set_folder_residency(
        port, folder, secret_key=secret_key, residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-direct.bin"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            # No gesture between binding the folder and here: the two devices
            # find each other on their own (outcome 2).
            _await_siblings(b, a, "before the write")
            before = _reading(b)

            (a.path / name).write_bytes(body)
            _await_bytes(b, name, body, "the direct transfer", a)

            after = _reading(b)
            served = after.get("chunks", 0) - before.get("chunks", 0)
            left = after.get("to_nest", 0) - before.get("to_nest", 0)
            assert served >= 1 and left == 0, (
                f"{name!r} reached seat b, but not straight from seat a: its agent's "
                f"sibling reading went {before!r} -> {after!r} ({served} chunk(s) "
                f"served by a sibling, {left} left to the nest). In a metadata-only "
                f"folder the only other road is the nest's relay, so the bytes "
                f"passed through the nest\n--- seat b ---\n{b.diagnostics()}\n"
                f"--- seat a ---\n{a.diagnostics()}"
            )
            assert after.get("bytes", 0) - before.get("bytes", 0) >= BODY_BYTES, (
                f"seat b's agent counts fewer sibling bytes than the file holds: "
                f"{before!r} -> {after!r}"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + SIBLING_WINDOW + 3 * WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_a_file_still_arrives_through_the_nest_when_the_direct_path_is_gone(
    nest_instance, request, seat_pair
):
    """The two seats had a direct path; seat ``a`` writes a file while ``b`` is
    away and then goes away itself. ``b`` comes back to a sibling it can no
    longer reach, and the file arrives anyway — through the nest, with no byte
    from a sibling and nothing asked of anyone."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"fallback-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-fallback.bin"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    # The engine's own `file uploaded` line, path redacted as the log scrub
    # writes it — logged once the byte plane accepted the file. (Not the Media
    # listing: it lists media kinds only, and this body is not one.)
    logged = _log_path(name)

    def uploaded() -> bool:
        return any(
            "file uploaded" in line and logged in line for line in a.agent_log_lines()
        )

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            # The direct path existed — the fallback below is a fallback.
            _await_siblings(b, a, "before the cut")

            b.stop_keeping_state()
            (a.path / name).write_bytes(body)
            wait_until(
                uploaded, WINDOW, interval=1.0,
                diagnose=lambda: f"seat a never uploaded {name!r}\n{a.diagnostics()}",
            )
            # Full residency: the bytes are on the nest now, so the nest no
            # longer needs seat a for them.
            a.stop()

            b.restart(STARTUP_WINDOW)
            _await_bytes(b, name, body, "the nest fallback")
            # Nothing asked and nothing warned (outcome 5): the file arrived by
            # the ordinary road with no error on the surface the user is looking
            # at. (The prompt/warning surface is `error-message`, convention 2.)
            shown = b.app.driver.get_text("error-message") if b.app.driver.is_visible(
                "error-message"
            ) else ""
            assert not shown, (
                f"seat b painted a message ({shown!r}) for a file that simply arrived "
                f"through the nest\n--- seat b ---\n{b.diagnostics()}"
            )
            reading = _reading(b)
            assert reading.get("chunks", 0) == 0, (
                f"seat b's agent counts chunks served by a sibling ({reading!r}), "
                f"but its only sibling was stopped before b came back"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _sequenced_rows(nest_instance, secret_key: str, folder: str, name: str) -> list:
    """The nest's sequenced change rows for ``name`` in ``folder``, matched by
    the row's only label, its convergent ``path_sealed``."""
    reply = sync_changes_list(
        nest_instance["port"], secret_key=secret_key, folder=folder,
        base_url=nest_instance["url"],
    )
    seal = sealed_path(secret_key, name)
    return [c for c in reply.get("changes") or [] if c.get("path_sealed") == seal]


def _row_summary(rows: list) -> str:
    """Who sequenced each row, and when — the fields that tell a writer's
    double upload from a sibling's re-upload, without the signature bytes."""
    keys = ("seq", "device_id", "change_type", "created_at", "manifest_hash")
    return repr([{k: r.get(k) for k in keys} for r in rows])


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + SIBLING_WINDOW + WINDOW + 2 * RETURN_WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_two_devices_keep_passing_a_file_while_the_nest_is_down(
    nest_instance, request, seat_pair
):
    """Outcome 7 (`p2p.md` § Same-account nest-down folder transfer). Two seats
    of one account hold a direct path; the nest goes down; a file written on
    seat ``a`` into a metadata-only folder lands on seat ``b`` — its change row
    AND its bytes from ``a``, since nothing else answers. Then the nest returns:
    ``a``'s own queued change is sequenced exactly once, attributed to ``a``
    (``b`` held only a provisional row, which never enters the nest's log), and
    ``b``'s copy stands with no conflict copy beside it."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"nestdown-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    reply = set_folder_residency(
        port, folder, secret_key=secret_key, residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-nestdown.bin"
    # Written on b once the nest is back: when ITS row is sequenced, b has run
    # an upload pass after the return — the barrier behind which a duplicate of
    # a's row from b would already be on the log.
    marker = f"{run_token}-nestdown-after.txt"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            _await_siblings(b, a, "before the nest went down")
            before = _reading(b)

            stop_nest(nest_instance, graceful=True)
            try:
                atomic_write(a.path / name, body)
                _await_bytes(b, name, body, "the nest-down transfer", a)

                after = _reading(b)
                served = after.get("chunks", 0) - before.get("chunks", 0)
                left = after.get("to_nest", 0) - before.get("to_nest", 0)
                assert served >= 1 and left == 0, (
                    f"{name!r} reached seat b with the nest down, but its agent's "
                    f"sibling reading went {before!r} -> {after!r} ({served} chunk(s) "
                    f"served by a sibling, {left} left to a nest that was not "
                    f"there)\n--- seat b ---\n{b.diagnostics()}\n"
                    f"--- seat a ---\n{a.diagnostics()}"
                )
                assert after.get("bytes", 0) - before.get("bytes", 0) >= BODY_BYTES, (
                    f"seat b's agent counts fewer sibling bytes than the file holds: "
                    f"{before!r} -> {after!r}"
                )
            finally:
                # The session-scoped nest must outlive this test: same port,
                # same data dir, no factory reset.
                start_nest_in_place(nest_instance)

            # The nest sequences a's queued change on its catch-up.
            wait_until(
                lambda: _sequenced_rows(nest_instance, secret_key, folder, name),
                RETURN_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"the nest came back but never sequenced {name!r} — seat a's "
                    f"offline change was not uploaded on its catch-up\n"
                    f"--- seat a ---\n{a.diagnostics()}\n--- seat b ---\n{b.diagnostics()}"
                ),
            )
            atomic_write(b.path / marker, "written after the nest came back\n")
            wait_until(
                lambda: _sequenced_rows(nest_instance, secret_key, folder, marker),
                RETURN_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seat b never uploaded {marker!r} after the nest came back\n"
                    f"--- seat b ---\n{b.diagnostics()}"
                ),
            )

            rows = _sequenced_rows(nest_instance, secret_key, folder, name)
            marker_device = _sequenced_rows(
                nest_instance, secret_key, folder, marker
            )[0].get("device_id")
            assert len(rows) == 1, (
                f"the nest sequenced {name!r} {len(rows)} times, not once: "
                f"{_row_summary(rows)} (seat b's device is {marker_device}) — a "
                f"provisional row a sibling served must never enter the nest's log "
                f"a second time\n--- seat a ---\n{a.diagnostics()}\n"
                f"--- seat b ---\n{b.diagnostics()}"
            )
            assert rows[0].get("device_id") != marker_device, (
                f"the nest's one row for {name!r} is attributed to seat b, which "
                f"only ever held it provisionally: {rows[0]!r}"
            )
            # Behind the barrier b is reconnected and has run a sync pass since
            # the return: a reconcile that dropped or forked its copy shows here.
            assert (b.path / name).read_bytes() == body, (
                f"seat b's copy of {name!r} did not stand once the nest's row "
                f"arrived\n--- seat b ---\n{b.diagnostics()}"
            )
            stray = sorted(
                p.name for p in b.path.iterdir()
                if p.name.startswith(run_token) and p.name not in (name, marker)
            )
            assert not stray, (
                f"seat b holds {stray!r} beside {name!r} — the nest's echo of a "
                f"row b already held provisionally forked it\n{b.diagnostics()}"
            )
            # A read that failed while the nest was down may paint "the
            # connection to the nest was lost" on the page (`folders.md`
            # § Errors & edge cases); once the nest answers again the reconnect
            # re-read retires it (`transport.md` § Push events, the reconnect
            # sweep). Telling the user the nest is unreachable while it answers
            # is the red.
            for s in (a, b):
                wait_until(
                    lambda s=s: not s.app.error_text().strip(),
                    RETURN_WINDOW, interval=2.0,
                    diagnose=lambda s=s: (
                        f"seat {s.name}'s error-message still reads "
                        f"{s.app.error_text()!r} after the nest came back and "
                        f"its agent reconnected\n{s.diagnostics()}"
                    ),
                )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

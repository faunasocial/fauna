"""A device you remove from your account can no longer fetch anything directly
from your other devices.

Owner: ``docs/goal/architecture/account-sync-plane.md`` § The admission seam →
*Validity and severance*: removing a device severs its peer-plane admission,
per request, on both directions, once the evaluating side's removed-device
snapshot has re-derived — and on the remover's own machine the removal write
asks the serving agent for that pass at once, so the remover severs within one
pass rather than a backstop tick.

**What this pins that the tier below cannot.** The mechanism is pinned store to
store one tier down (``fauna-sync-engine/tests/peer_leg_convergence.rs``'s
severance pins; ``peer_dial_convergence.rs``'s
``a_removed_sibling_leaves_the_registry_at_the_next_pass``). What none of them
can answer is whether the removal a user makes on the Devices page — written by
the APP's runtime — reaches the per-user sync agent that holds the machine's
engine role and serves the chunks (``account-runtime.md`` § Multi-instance
concurrency → *The agent holds the role when present*). A two-seat run once
found it did not, for minutes: the removed seat kept pulling bodies directly.

**The observables** are each agent's own reading of the plane — its
``GetServiceStatus`` reply's ``peer_files`` (``siblings``, ``chunks``):

1. seat ``a`` (the remover) holds no sibling — its dial pass no longer reaches
   the removed seat, and that same pass re-derived the snapshot its serve side
   refuses from (the refresh runs before the dial);
2. seat ``b`` (the removed one) holds no sibling — ``a`` is still a verified
   member in ``b``'s eyes, so ``b`` keeps dialing it, and only ``a``'s refusal
   empties ``b``'s registry;
3. a file ``a`` then writes still reaches ``b`` through the nest (a full
   residency folder, so the nest holds its bytes), and ``b``'s count of chunks
   a sibling served it does not move.

Two tui seats on one machine, no operator (convention 16); the removal is the
user's gesture on ``a``'s Devices page (convention 8). Every wait is a deadline
poll on state (convention 14).
"""

import contextlib
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from common import user_create_folder
from helpers import sync_seats
from helpers.waiting import await_device_removal_ready, wait_until
from tests.test_device_direct_transfer_switch import _card_scope
from tests.test_device_to_device_file_transfer import (
    BODY_BYTES,
    SIBLING_WINDOW,
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _await_bytes,
    _await_siblings,
    _reading,
    _seat_factory,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
]

# The tui pair only: the journey drives the Devices page through the seat's
# own action layer, and tui is the lead app.
_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(sys.platform, (("tui", "tui"),))
]

#: The gesture's own re-snapshot repaints the roster.
_ROSTER_WINDOW = 60.0


def _sibling_card(seat) -> str | None:
    """The scope of the other seat's card on ``seat``'s Devices page, or None
    once it has left the roster."""
    seat.app.backups.navigate_devices()
    try:
        return _card_scope(seat.app.driver, own=False)
    except AssertionError:
        return None


def _await_no_sibling(seat, other, what: str) -> None:
    wait_until(
        lambda: _reading(seat).get("siblings", 0) == 0,
        SIBLING_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"{what}: seat {seat.name}'s agent still holds an admitted sibling "
            f"(last reading {seat.peer_files()!r})\n"
            f"--- seat {seat.name} ---\n{seat.diagnostics()}\n"
            f"--- seat {other.name} ---\n{other.diagnostics()}"
        ),
    )


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * SIBLING_WINDOW + _ROSTER_WINDOW + WINDOW + 120))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_a_removed_device_fetches_nothing_directly_from_your_other_devices(
    nest_instance, request, seat_pair
):
    """Two seats with a direct path between them; seat ``a`` removes seat ``b``
    on its Devices page. Both agents drop the channel, and a file ``a`` writes
    afterwards reaches ``b`` only through the nest — no chunk from a sibling."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"severed-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-after-removal.bin"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            a.await_ready(STARTUP_WINDOW)
            b.await_ready(STARTUP_WINDOW)
            # The direct path existed, both ways — the severance below severs
            # something.
            _await_siblings(b, a, "before the removal")
            _await_siblings(a, b, "before the removal")

            # The user's gesture: seat a removes seat b's card.
            driver = a.app.driver
            a.app.backups.navigate_devices()
            await_device_removal_ready(driver)
            scope = _card_scope(driver, own=False)
            index = int(scope.rsplit("[", 1)[1].rstrip("]"))
            driver.click("device-remove-button", index=index)
            wait_until(
                lambda: _sibling_card(a) is None,
                _ROSTER_WINDOW, interval=1.0,
                diagnose=lambda: (
                    f"seat b's card never left seat a's Devices page after the "
                    f"removal (error-message "
                    f"{driver.get_text('error-message') if driver.is_visible('error-message') else ''!r}; "
                    f"{driver.diagnose('device-card')})"
                ),
            )

            # (1) the remover's agent stopped dialing b, on the pass that
            # re-derived its serve side's refusal; (2) b's dial is refused.
            _await_no_sibling(a, b, "after the removal, on the remover")
            _await_no_sibling(b, a, "after the removal, on the removed device")
            before = _reading(b)

            # (3) a file written now reaches b by the nest alone.
            (a.path / name).write_bytes(body)
            _await_bytes(b, name, body, "the post-removal file", a)
            after = _reading(b)
            served = after.get("chunks", 0) - before.get("chunks", 0)
            assert served == 0 and after.get("siblings", 0) == 0, (
                f"seat b was removed, yet its agent's sibling reading went {before!r} "
                f"-> {after!r} ({served} chunk(s) served by a sibling): a removed "
                f"device fetched directly from its former sibling\n"
                f"--- seat b ---\n{b.diagnostics()}\n--- seat a ---\n{a.diagnostics()}"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
